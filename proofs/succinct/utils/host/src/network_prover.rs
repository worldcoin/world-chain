//! Range proofs are produced in `Compressed` mode so the aggregation guest can recursively
//! verify them; the aggregation proof mode is configurable (Groth16 for on-chain verification).

use crate::SuccinctProverError;
use alloy_primitives::B256;
use anyhow::{Context, bail};
use async_trait::async_trait;
pub use sp1_sdk::SP1ProofMode;
use sp1_sdk::{
    HashableKey, NetworkProver, ProveRequest, Prover, ProverClient, ProvingKey, SP1Proof,
    SP1ProofWithPublicValues, SP1ProvingKey, SP1Stdin,
    network::proto::{GetProofRequestStatusResponse, types::FulfillmentStatus},
};
use world_chain_proof_core::{
    artifacts::{AggregationProofArtifact, ProofArtifact, RangeProofArtifact},
    boot::BootInfoStruct,
    types::{AggregationInputs, AggregationOutputs},
};
use world_chain_proof_succinct_utils::{
    AggregationProofRequest, ProofRequest, RangeProofRequest, Sp1SessionStatus, WorldSuccinctProver,
};
use world_chain_prover_service::{ProofRequestId, SessionType};

/// [`WorldSuccinctProver`] network implementation over the sp1-sdk network prover.
pub struct NetworkSuccinctProver {
    client: NetworkProver,
    range_pk: SP1ProvingKey,
    agg_pk: SP1ProvingKey,
    multi_block_vkey: [u32; 8],
    agg_mode: SP1ProofMode,
}

impl NetworkSuccinctProver {
    /// Creates the prover using caller-supplied ELFs. Use this in production binaries with
    /// ELFs embedded at compile time via `world_chain_proof_succinct_elfs`.
    pub async fn new(agg_mode: SP1ProofMode, private_key: &str) -> anyhow::Result<Self> {
        let range_elf = world_chain_proof_succinct_elfs::range_elf();
        let agg_elf = world_chain_proof_succinct_elfs::aggregation_elf();
        let client = ProverClient::builder()
            .network()
            .private_key(private_key)
            .build()
            .await;
        let range_pk = client
            .setup(range_elf.clone())
            .await
            .context("range program setup failed")?;
        let agg_pk = client
            .setup(agg_elf.clone())
            .await
            .context("aggregation program setup failed")?;
        let multi_block_vkey = range_pk.verifying_key().hash_u32();

        Ok(Self {
            client,
            range_pk,
            agg_pk,
            multi_block_vkey,
            agg_mode,
        })
    }

    async fn request_range_proof(&self, request: RangeProofRequest) -> anyhow::Result<String> {
        let mut stdin = SP1Stdin::new();
        stdin.write_vec(request.witness_rkyv);

        let backend_session_id = self
            .client
            .prove(&self.range_pk, stdin)
            .compressed()
            .request()
            .await
            .context("request range proving failed")?;

        Ok(backend_session_id.to_string())
    }

    async fn request_aggregation_proof(
        &self,
        request: AggregationProofRequest,
    ) -> anyhow::Result<String> {
        let mut stdin = SP1Stdin::new();
        let range_vk = self.range_pk.verifying_key().vk.clone();
        for proof_bytes in &request.range_proofs {
            let proof: sp1_sdk::SP1ProofWithPublicValues =
                bincode::deserialize(proof_bytes).context("range proof deserialization failed")?;
            let SP1Proof::Compressed(inner) = proof.proof else {
                return Err(SuccinctProverError::NotCompressed.into());
            };
            stdin.write_proof(*inner, range_vk.clone());
        }
        stdin.write(&request.inputs);
        stdin.write_vec(request.l1_headers_cbor);

        let backend_session_id = self
            .client
            .prove(&self.agg_pk, stdin)
            .mode(self.agg_mode)
            .request()
            .await
            .context("aggregation proving failed")?;

        Ok(backend_session_id.to_string())
    }

    /// Fetch the network session state and any proof returned by the SP1 Network.
    pub async fn get_network_proof_status(
        &self,
        backend_session_id: &str,
    ) -> anyhow::Result<(Sp1SessionStatus, Option<SP1ProofWithPublicValues>)> {
        let proof_id = parse_proof_id(backend_session_id)?;
        let (status, proof) = self
            .client
            .get_proof_status(proof_id)
            .await
            .context("failed to get network proof status")?;
        let sp1_status = sp1_status(&status);
        Ok((sp1_status, proof))
    }
}

#[async_trait]
impl WorldSuccinctProver for NetworkSuccinctProver {
    fn supports_persistent_sessions(&self) -> bool {
        true
    }

    async fn submit(
        &self,
        _proof_id: ProofRequestId,
        session_type: SessionType,
        request: ProofRequest,
    ) -> anyhow::Result<String> {
        match session_type {
            SessionType::Stark => {
                let ProofRequest::Range(range_request) = request else {
                    bail!("proof request and session type mistmach")
                };
                let backend_session_id = self.request_range_proof(range_request).await?;
                Ok(backend_session_id)
            }
            SessionType::Snark => {
                let ProofRequest::Aggregation(session_request) = request else {
                    bail!("proof request and session type mistmach")
                };
                let agg_request = AggregationProofRequest {
                    inputs: AggregationInputs {
                        boot_infos: session_request.boot_infos,
                        latest_l1_checkpoint_head: session_request.latest_l1_checkpoint_head,
                        multi_block_vkey: self.multi_block_vkey,
                        prover_address: session_request.prover_address,
                    },
                    l1_headers_cbor: session_request.l1_headers_cbor,
                    range_proofs: session_request.range_proofs,
                };
                let backend_session_id = self.request_aggregation_proof(agg_request).await?;
                Ok(backend_session_id.to_string())
            }
        }
    }

    async fn poll(
        &self,
        session_id: String,
        _session_type: SessionType,
    ) -> anyhow::Result<Sp1SessionStatus> {
        let (sp1_status, _maybe_proof) = self.get_network_proof_status(&session_id).await?;
        Ok(sp1_status)
    }

    async fn download(
        &self,
        session_id: String,
        session_type: SessionType,
    ) -> anyhow::Result<ProofArtifact> {
        let (sp1_status, maybe_proof) = self.get_network_proof_status(&session_id).await?;
        match sp1_status {
            Sp1SessionStatus::Completed => {
                let proof = maybe_proof.ok_or_else(|| {
                    anyhow::anyhow!(
                        "network proof {session_id} is fulfilled but no proof was returned"
                    )
                })?;
                match session_type {
                    SessionType::Stark => {
                        let boot_info: BootInfoStruct =
                            bincode::deserialize(proof.public_values.as_slice())
                                .context("range proof public values deserialization failed")?;

                        let proof_bytes = bincode::serialize(&proof)
                            .context("range proof serialization failed")?;

                        Ok(ProofArtifact::Range(RangeProofArtifact {
                            boot_info,
                            proof: proof_bytes,
                        }))
                    }
                    SessionType::Snark => {
                        let outputs =
                            <AggregationOutputs as alloy_sol_types::SolValue>::abi_decode(
                                proof.public_values.as_slice(),
                            )
                            .context("aggregation outputs abi decoding failed")?;

                        // Groth16/Plonk proofs serialize to their on-chain calldata representation; other
                        // modes (mock runs, compressed) keep the full sdk proof for offline use.
                        let proof_bytes = match &proof.proof {
                            SP1Proof::Groth16(_) | SP1Proof::Plonk(_) => proof.bytes(),
                            _ => bincode::serialize(&proof)
                                .context("aggregation proof serialization failed")?,
                        };
                        Ok(ProofArtifact::Aggregation(AggregationProofArtifact {
                            outputs,
                            proof: proof_bytes,
                        }))
                    }
                }
            }
            Sp1SessionStatus::Running => {
                bail!("network proof {session_id} is not fulfilled yet");
            }
            Sp1SessionStatus::Failed(reason) => bail!("{reason}"),
            Sp1SessionStatus::NotFound => {
                bail!("network proof {session_id} was not found");
            }
        }
    }
}

/// Parse a network proof ID from its hex string representation.
fn parse_proof_id(proof_id: &str) -> anyhow::Result<B256> {
    proof_id
        .parse::<B256>()
        .map_err(|e| anyhow::anyhow!("invalid network proof ID: {e}"))
}

/// Map an SP1 Network proof status response to the sp1 session status.
fn sp1_status(status: &GetProofRequestStatusResponse) -> Sp1SessionStatus {
    match FulfillmentStatus::try_from(status.fulfillment_status()) {
        Ok(FulfillmentStatus::Fulfilled) => Sp1SessionStatus::Completed,
        Ok(FulfillmentStatus::Unfulfillable) => Sp1SessionStatus::Failed(format!(
            "proof unfulfillable, execution_status={}",
            status.execution_status()
        )),
        Ok(FulfillmentStatus::Assigned)
        | Ok(FulfillmentStatus::Requested)
        | Ok(FulfillmentStatus::UnspecifiedFulfillmentStatus) => Sp1SessionStatus::Running,
        Err(_) => Sp1SessionStatus::Failed(format!(
            "unknown network proof fulfillment status: {}",
            status.fulfillment_status()
        )),
    }
}
