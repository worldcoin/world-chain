//! Host-side `NitroProver` that talks to a running Nitro enclave over vsock.

use k256::ecdsa::{RecoveryId, Signature as K256Signature, VerifyingKey};
use tokio_vsock::{VsockAddr, VsockStream};
use tracing::{debug, instrument, warn};
use world_chain_proof_core::boot::BootInfoStruct;

use crate::{
    ExpectedPcrs, NitroAggregationProofArtifact, NitroAggregationProofRequest,
    NitroRangeProofArtifact, NitroRangeProofRequest, WorldTeeProver,
    attestation::{self, AttestationError},
    protocol::{
        self, DEFAULT_VSOCK_PORT, EnclaveRequest, EnclaveResponse, FrameError, PROTOCOL_VERSION,
    },
};

/// Address of a running enclave reachable on the host's vsock interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnclaveEndpoint {
    /// Nitro enclave vsock context id.
    pub cid: u32,
    /// vsock port the enclave is listening on.
    pub port: u32,
}

impl EnclaveEndpoint {
    /// Creates an endpoint with the supplied CID using the default protocol port.
    #[must_use]
    pub const fn new(cid: u32) -> Self {
        Self {
            cid,
            port: DEFAULT_VSOCK_PORT,
        }
    }

    /// Creates an endpoint with a fully-specified port.
    #[must_use]
    pub const fn with_port(cid: u32, port: u32) -> Self {
        Self { cid, port }
    }
}

/// Host-side prover that proxies range and aggregation requests to a Nitro enclave.
#[derive(Clone, Debug)]
pub struct NitroProver {
    endpoint: EnclaveEndpoint,
    expected_pcrs: ExpectedPcrs,
    runtime: tokio::runtime::Handle,
}

impl NitroProver {
    /// Creates a new prover bound to the current tokio runtime.
    ///
    /// # Panics
    /// Panics if no tokio runtime is currently active. Use [`NitroProver::with_runtime`]
    /// when calling from a non-async context.
    #[must_use]
    pub fn new(endpoint: EnclaveEndpoint, expected_pcrs: ExpectedPcrs) -> Self {
        Self::with_runtime(endpoint, expected_pcrs, tokio::runtime::Handle::current())
    }

    /// Creates a new prover with an explicit tokio runtime handle.
    #[must_use]
    pub fn with_runtime(
        endpoint: EnclaveEndpoint,
        expected_pcrs: ExpectedPcrs,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            endpoint,
            expected_pcrs,
            runtime,
        }
    }

    /// Sends a request to the enclave and returns the raw enclave response.
    async fn round_trip(
        &self,
        request: EnclaveRequest,
    ) -> Result<EnclaveResponse, NitroProverError> {
        let addr = VsockAddr::new(self.endpoint.cid, self.endpoint.port);
        debug!(target: "world_chain::nitro", cid = self.endpoint.cid, port = self.endpoint.port, "connecting to enclave");

        let mut stream = VsockStream::connect(addr)
            .await
            .map_err(NitroProverError::Connect)?;
        protocol::write_frame(&mut stream, &request).await?;
        let response: EnclaveResponse = protocol::read_frame(&mut stream).await?;
        Ok(response)
    }

    /// Requests the enclave's NSM attestation document that embeds its ephemeral public key.
    ///
    /// Use this during one-time registration to learn the enclave's secp256k1 public key
    /// and verify it is pinned to the expected PCR measurements.
    #[instrument(skip_all, fields(endpoint = ?self.endpoint))]
    pub async fn get_attestation_async(
        &self,
    ) -> Result<(Vec<u8>, Vec<u8>), NitroProverError> {
        let response = self.round_trip(EnclaveRequest::GetAttestation).await?;
        match response {
            EnclaveResponse::Attestation {
                attestation_doc,
                public_key,
            } => Ok((attestation_doc, public_key)),
            EnclaveResponse::Error { message } => Err(NitroProverError::Enclave(message)),
            _ => Err(NitroProverError::UnexpectedResponse("non-attestation")),
        }
    }

    /// Async version of [`WorldTeeProver::prove_range`].
    #[instrument(skip_all, fields(endpoint = ?self.endpoint))]
    pub async fn prove_range_async(
        &self,
        request: NitroRangeProofRequest,
    ) -> Result<NitroRangeProofArtifact, NitroProverError> {
        // ── Step 1: Obtain and verify the enclave's certified ephemeral public key ──
        //
        // When real PCRs are configured we fetch the key-attestation document first so
        // that the proof signature can be bound to the same AWS-certified key.  Skipped
        // in placeholder / dev mode with an explicit warning.
        let certified_pub_key: Option<Vec<u8>> = if self.expected_pcrs.is_placeholder() {
            warn!(
                target: "world_chain::nitro",
                "placeholder PCRs in use — skipping attestation and signature verification (dev/test mode only)"
            );
            None
        } else {
            let (attest_doc, pub_key) = self.get_attestation_async().await?;
            // Verify COSE_Sign1 signature on the key-attestation document.
            attestation::verify_cose_sign1_signature(&attest_doc)?;
            // Verify PCR binding (no user_data for GetAttestation documents).
            attestation::verify_pcrs_only(&attest_doc, &self.expected_pcrs)?;
            Some(pub_key)
        };

        // ── Step 2: Prove ──
        let enclave_request = EnclaveRequest::Range {
            version: PROTOCOL_VERSION,
            witness_rkyv: request.witness_rkyv,
            expected_public_values: request.expected_public_values,
        };
        let response = self.round_trip(enclave_request).await?;
        let (boot_info, attestation_doc, signature) = match response {
            EnclaveResponse::Range {
                boot_info,
                attestation_doc,
                signature,
            } => (boot_info, attestation_doc, signature),
            EnclaveResponse::Error { message } => return Err(NitroProverError::Enclave(message)),
            EnclaveResponse::Aggregation { .. } => {
                return Err(NitroProverError::UnexpectedResponse("aggregation"));
            }
            EnclaveResponse::Attestation { .. } => {
                return Err(NitroProverError::UnexpectedResponse("attestation"));
            }
        };

        // ── Step 3: Verify range attestation doc + proof signature ──
        if let Some(ref expected_pub_key) = certified_pub_key {
            let expected_user_data = protocol::range_user_data(&boot_info);
            attestation::parse_check_and_verify(
                &attestation_doc,
                &self.expected_pcrs,
                &expected_user_data,
            )?;
            verify_proof_signature(&signature, &boot_info, expected_pub_key)?;
        }

        Ok(NitroRangeProofArtifact {
            boot_info,
            attestation_doc,
            signature,
        })
    }

    /// Async version of [`WorldTeeProver::prove_aggregation`].
    #[instrument(skip_all, fields(endpoint = ?self.endpoint))]
    pub async fn prove_aggregation_async(
        &self,
        request: NitroAggregationProofRequest,
    ) -> Result<NitroAggregationProofArtifact, NitroProverError> {
        // ── Step 1: Obtain and verify the enclave's certified ephemeral public key ──
        let certified_pub_key: Option<Vec<u8>> = if self.expected_pcrs.is_placeholder() {
            warn!(
                target: "world_chain::nitro",
                "placeholder PCRs in use — skipping attestation and signature verification (dev/test mode only)"
            );
            None
        } else {
            let (attest_doc, pub_key) = self.get_attestation_async().await?;
            attestation::verify_cose_sign1_signature(&attest_doc)?;
            attestation::verify_pcrs_only(&attest_doc, &self.expected_pcrs)?;
            Some(pub_key)
        };

        // ── Step 2: Prove ──
        let enclave_request = EnclaveRequest::Aggregation {
            version: PROTOCOL_VERSION,
            inputs: request.inputs.clone(),
            l1_headers_cbor: request.l1_headers_cbor,
        };
        let response = self.round_trip(enclave_request).await?;
        let (boot_info, attestation_doc, signature) = match response {
            EnclaveResponse::Aggregation {
                boot_info,
                attestation_doc,
                signature,
            } => (boot_info, attestation_doc, signature),
            EnclaveResponse::Error { message } => return Err(NitroProverError::Enclave(message)),
            EnclaveResponse::Range { .. } => {
                return Err(NitroProverError::UnexpectedResponse("range"));
            }
            EnclaveResponse::Attestation { .. } => {
                return Err(NitroProverError::UnexpectedResponse("attestation"));
            }
        };

        // ── Step 3: Verify aggregation attestation doc + proof signature ──
        if let Some(ref expected_pub_key) = certified_pub_key {
            let expected_user_data = protocol::aggregation_user_data(&boot_info, &request.inputs);
            attestation::parse_check_and_verify(
                &attestation_doc,
                &self.expected_pcrs,
                &expected_user_data,
            )?;
            verify_proof_signature(&signature, &boot_info, expected_pub_key)?;
        }

        // The aggregation artifact carries the attestation document as `proof` and
        // the enclave's verified secp256k1 signature so callers can perform
        // EVM-native on-chain recovery without re-requesting the signature.
        Ok(NitroAggregationProofArtifact {
            outputs: aggregation_outputs(&boot_info, &request.inputs),
            proof: attestation_doc,
            signature,
        })
    }
}

/// Verifies the enclave's 65-byte recoverable secp256k1 signature over
/// `signing_commitment(boot_info)` and checks that the recovered key matches
/// `expected_pub_key` (the compressed SEC1-encoded public key from the key-attestation
/// document).
///
/// # Errors
///
/// Returns [`NitroProverError::InvalidSignature`] if the signature bytes are malformed.
/// Returns [`NitroProverError::SignatureMismatch`] if the recovered key differs.
fn verify_proof_signature(
    signature: &[u8],
    boot_info: &BootInfoStruct,
    expected_pub_key: &[u8],
) -> Result<(), NitroProverError> {
    if signature.len() != 65 {
        return Err(NitroProverError::InvalidSignature(format!(
            "expected 65 bytes, got {}",
            signature.len()
        )));
    }
    let commitment = protocol::signing_commitment(boot_info);
    let sig = K256Signature::from_slice(&signature[..64])
        .map_err(|e| NitroProverError::InvalidSignature(e.to_string()))?;
    let rec_id = RecoveryId::from_byte(signature[64]).ok_or_else(|| {
        NitroProverError::InvalidSignature(format!(
            "invalid secp256k1 recovery id byte: {}",
            signature[64]
        ))
    })?;
    let recovered_vk = VerifyingKey::recover_from_prehash(&commitment, &sig, rec_id)
        .map_err(|e| NitroProverError::InvalidSignature(e.to_string()))?;
    let recovered_key_bytes = recovered_vk.to_encoded_point(true).as_bytes().to_vec();
    if recovered_key_bytes.as_slice() != expected_pub_key {
        return Err(NitroProverError::SignatureMismatch {
            recovered: hex::encode(&recovered_key_bytes),
            expected: hex::encode(expected_pub_key),
        });
    }
    Ok(())
}

fn aggregation_outputs(
    boot_info: &world_chain_proof_core::boot::BootInfoStruct,
    inputs: &world_chain_proof_core::types::AggregationInputs,
) -> world_chain_proof_core::types::AggregationOutputs {
    use alloy_primitives::B256;
    use world_chain_proof_core::types::{AggregationOutputs, u32_to_u8};
    AggregationOutputs {
        l1Head: boot_info.l1Head,
        l2PreRoot: boot_info.l2PreRoot,
        l2PostRoot: boot_info.l2PostRoot,
        l2BlockNumber: boot_info.l2BlockNumber,
        rollupConfigHash: boot_info.rollupConfigHash,
        multiBlockVKey: B256::from(u32_to_u8(inputs.multi_block_vkey)),
        proverAddress: inputs.prover_address,
    }
}

impl WorldTeeProver for NitroProver {
    type Error = NitroProverError;

    fn prove_range(
        &self,
        request: NitroRangeProofRequest,
    ) -> Result<NitroRangeProofArtifact, Self::Error> {
        let runtime = self.runtime.clone();
        let prover = self.clone();
        runtime.block_on(prover.prove_range_async(request))
    }

    fn prove_aggregation(
        &self,
        request: NitroAggregationProofRequest,
    ) -> Result<NitroAggregationProofArtifact, Self::Error> {
        let runtime = self.runtime.clone();
        let prover = self.clone();
        runtime.block_on(prover.prove_aggregation_async(request))
    }
}

/// Errors raised by the host-side Nitro prover.
#[derive(Debug, thiserror::Error)]
pub enum NitroProverError {
    /// Failed to connect to the enclave over vsock.
    #[error("failed to connect to nitro enclave: {0}")]
    Connect(#[source] std::io::Error),
    /// Wire-framing error.
    #[error(transparent)]
    Frame(#[from] FrameError),
    /// Enclave returned a structured error message.
    #[error("nitro enclave returned error: {0}")]
    Enclave(String),
    /// Attestation document verification failed.
    #[error(transparent)]
    Attestation(#[from] AttestationError),
    /// Enclave returned a response variant we did not request.
    #[error("nitro enclave returned unexpected response variant: {0}")]
    UnexpectedResponse(&'static str),
    /// Proof signature bytes are structurally invalid (wrong length, bad encoding, …).
    #[error("proof secp256k1 signature is invalid: {0}")]
    InvalidSignature(String),
    /// Proof signature recovered a different public key than the certified enclave key.
    #[error(
        "proof signature key mismatch: recovered 0x{recovered} != enclave 0x{expected}"
    )]
    SignatureMismatch { recovered: String, expected: String },
}
