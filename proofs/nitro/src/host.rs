//! Host-side `NitroProver` that talks to a running Nitro enclave over vsock.

use tokio_vsock::{VsockAddr, VsockStream};
use tracing::{debug, instrument};

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

    /// Async version of [`WorldTeeProver::prove_range`].
    #[instrument(skip_all, fields(endpoint = ?self.endpoint))]
    pub async fn prove_range_async(
        &self,
        request: NitroRangeProofRequest,
    ) -> Result<NitroRangeProofArtifact, NitroProverError> {
        let enclave_request = EnclaveRequest::Range {
            version: PROTOCOL_VERSION,
            witness_rkyv: request.witness_rkyv,
            expected_public_values: request.expected_public_values,
        };
        let response = self.round_trip(enclave_request).await?;
        let (boot_info, attestation_doc) = match response {
            EnclaveResponse::Range {
                boot_info,
                attestation_doc,
            } => (boot_info, attestation_doc),
            EnclaveResponse::Error { message } => return Err(NitroProverError::Enclave(message)),
            EnclaveResponse::Aggregation { .. } => {
                return Err(NitroProverError::UnexpectedResponse("aggregation"));
            }
        };

        let expected_user_data = protocol::range_user_data(&boot_info);
        attestation::parse_and_check_pcrs(
            &attestation_doc,
            &self.expected_pcrs,
            &expected_user_data,
        )?;

        Ok(NitroRangeProofArtifact {
            boot_info,
            attestation_doc,
        })
    }

    /// Async version of [`WorldTeeProver::prove_aggregation`].
    #[instrument(skip_all, fields(endpoint = ?self.endpoint))]
    pub async fn prove_aggregation_async(
        &self,
        request: NitroAggregationProofRequest,
    ) -> Result<NitroAggregationProofArtifact, NitroProverError> {
        let enclave_request = EnclaveRequest::Aggregation {
            version: PROTOCOL_VERSION,
            inputs: request.inputs.clone(),
            l1_headers_cbor: request.l1_headers_cbor,
        };
        let response = self.round_trip(enclave_request).await?;
        let (boot_info, attestation_doc) = match response {
            EnclaveResponse::Aggregation {
                boot_info,
                attestation_doc,
            } => (boot_info, attestation_doc),
            EnclaveResponse::Error { message } => return Err(NitroProverError::Enclave(message)),
            EnclaveResponse::Range { .. } => {
                return Err(NitroProverError::UnexpectedResponse("range"));
            }
        };

        let expected_user_data = protocol::aggregation_user_data(&boot_info, &request.inputs);
        attestation::parse_and_check_pcrs(
            &attestation_doc,
            &self.expected_pcrs,
            &expected_user_data,
        )?;

        // The aggregation artifact mirrors the Succinct shape, but the `proof` bytes carry
        // the attestation document instead of an SP1 proof.
        Ok(
            world_chain_proof_core::artifacts::AggregationProofArtifact {
                outputs: aggregation_outputs(&boot_info, &request.inputs),
                proof: attestation_doc,
            },
        )
    }
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
}
