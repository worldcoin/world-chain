//! Range proofs are produced in `Compressed` mode so the aggregation guest can recursively
//! verify them; the aggregation proof mode is configurable (Groth16 for on-chain verification).

use crate::cpu_prover::SuccinctProverError;
use anyhow::{Context, bail};
use async_trait::async_trait;
use sp1_sdk::{
    HashableKey, MockProver, ProveRequest, Prover, ProvingKey, SP1Proof, SP1ProofMode,
    SP1ProvingKey, SP1Stdin,
};
use std::{collections::HashMap, sync::Mutex};
use world_chain_proof_core::{
    artifacts::ProofArtifact,
    boot::BootInfoStruct,
    types::{AggregationInputs, AggregationOutputs},
};
use world_chain_proof_succinct_utils::{
    AggregationProofArtifact, AggregationProofRequest, ProofRequest, RangeProofArtifact,
    RangeProofRequest, Sp1SessionStatus, WorldSuccinctProver,
};
use world_chain_prover_service::{ProofRequestId, SessionType};

pub struct MockSuccinctProver {
    client: MockProver,
    range_pk: SP1ProvingKey,
    agg_pk: SP1ProvingKey,
    multi_block_vkey: [u32; 8],
    agg_mode: SP1ProofMode,

    range_proofs: Mutex<HashMap<String, RangeProofArtifact>>,
    agg_proofs: Mutex<HashMap<String, AggregationProofArtifact>>,
}

impl MockSuccinctProver {
    /// Creates the prover using caller-supplied ELFs. Use this in production binaries with
    /// ELFs embedded at compile time via `world_chain_proof_succinct_elfs`.
    pub async fn new(agg_mode: SP1ProofMode) -> anyhow::Result<Self> {
        let range_elf = world_chain_proof_succinct_elfs::range_elf();
        let agg_elf = world_chain_proof_succinct_elfs::aggregation_elf();
        let client = MockProver::new().await;
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
            range_proofs: Mutex::new(HashMap::default()),
            agg_proofs: Mutex::new(HashMap::default()),
        })
    }

    async fn prove_range(&self, request: RangeProofRequest) -> anyhow::Result<RangeProofArtifact> {
        let mut stdin = SP1Stdin::new();
        stdin.write_vec(request.witness_rkyv);

        let proof = self
            .client
            .prove(&self.range_pk, stdin)
            .compressed()
            .deferred_proof_verification(false)
            .await
            .context("range proving failed")?;

        let boot_info: BootInfoStruct = bincode::deserialize(proof.public_values.as_slice())
            .context("range proof public values deserialization failed")?;

        if let Some(expected) = request.expected_public_values {
            let expected_boot = BootInfoStruct::from(expected.boot_info);
            if expected_boot != boot_info {
                return Err(SuccinctProverError::BootInfoMismatch {
                    expected: Box::new(expected_boot),
                    actual: Box::new(boot_info),
                }
                .into());
            }
        }

        let proof_bytes = bincode::serialize(&proof).context("range proof serialization failed")?;

        Ok(RangeProofArtifact {
            boot_info,
            proof: proof_bytes,
        })
    }

    async fn prove_aggregation(
        &self,
        request: AggregationProofRequest,
    ) -> anyhow::Result<AggregationProofArtifact> {
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

        let proof = self
            .client
            .prove(&self.agg_pk, stdin)
            .mode(self.agg_mode)
            .deferred_proof_verification(false)
            .await
            .context("aggregation proving failed")?;

        self.client
            .verify(&proof, self.agg_pk.verifying_key(), None)
            .context("aggregation proof verification failed")?;

        let outputs = <AggregationOutputs as alloy_sol_types::SolValue>::abi_decode(
            proof.public_values.as_slice(),
        )
        .context("aggregation outputs abi decoding failed")?;

        // Groth16/Plonk proofs serialize to their on-chain calldata representation; other
        // modes (mock runs, compressed) keep the full sdk proof for offline use.
        let proof_bytes = match &proof.proof {
            SP1Proof::Groth16(_) | SP1Proof::Plonk(_) => proof.bytes(),
            _ => bincode::serialize(&proof).context("aggregation proof serialization failed")?,
        };

        Ok(AggregationProofArtifact {
            outputs,
            proof: proof_bytes,
        })
    }
}

#[async_trait]
impl WorldSuccinctProver for MockSuccinctProver {
    fn supports_persistent_sessions(&self) -> bool {
        false
    }

    async fn submit(
        &self,
        proof_id: ProofRequestId,
        session_type: SessionType,
        request: ProofRequest,
    ) -> anyhow::Result<String> {
        match session_type {
            SessionType::Stark => {
                let ProofRequest::Range(range_request) = request else {
                    bail!("proof request and session type mistmach")
                };
                let range_proof = self.prove_range(range_request).await?;
                let mut range_proofs = self
                    .range_proofs
                    .lock()
                    .map_err(|_| anyhow::anyhow!("mutex lock poisoned"))?;
                range_proofs.insert(proof_id.to_string(), range_proof);
                Ok(proof_id.to_string())
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
                let agg_proof = self.prove_aggregation(agg_request).await?;
                let mut agg_proofs = self
                    .agg_proofs
                    .lock()
                    .map_err(|_| anyhow::anyhow!("mutex lock poisoned"))?;
                agg_proofs.insert(proof_id.to_string(), agg_proof);
                Ok(proof_id.to_string())
            }
        }
    }

    async fn poll(
        &self,
        session_id: String,
        session_type: SessionType,
    ) -> anyhow::Result<Sp1SessionStatus> {
        match session_type {
            SessionType::Stark => {
                let range_proofs = self
                    .range_proofs
                    .lock()
                    .map_err(|_| anyhow::anyhow!("mutex lock poisoned"))?;
                if range_proofs.contains_key(&session_id) {
                    // CPU prover immediately returns completed status if the entry exists in the hashmap
                    Ok(Sp1SessionStatus::Completed)
                } else {
                    Ok(Sp1SessionStatus::NotFound)
                }
            }
            SessionType::Snark => {
                let agg_proofs = self
                    .agg_proofs
                    .lock()
                    .map_err(|_| anyhow::anyhow!("mutex lock poisoned"))?;
                if agg_proofs.contains_key(&session_id) {
                    // CPU prover immediately returns completed status if the entry exists in the hashmap
                    Ok(Sp1SessionStatus::Completed)
                } else {
                    Ok(Sp1SessionStatus::NotFound)
                }
            }
        }
    }

    async fn download(
        &self,
        session_id: String,
        session_type: SessionType,
    ) -> anyhow::Result<ProofArtifact> {
        match session_type {
            SessionType::Stark => {
                let range_proofs = self
                    .range_proofs
                    .lock()
                    .map_err(|_| anyhow::anyhow!("mutex lock poisoned"))?;
                let range_proof = range_proofs.get(&session_id).ok_or_else(|| {
                    anyhow::anyhow!("no range proof found for session_id: {}", session_id)
                })?;
                Ok(ProofArtifact::Range(range_proof.clone()))
            }
            SessionType::Snark => {
                let agg_proofs = self
                    .agg_proofs
                    .lock()
                    .map_err(|_| anyhow::anyhow!("mutex lock poisoned"))?;
                let agg_proof = agg_proofs.get(&session_id).ok_or_else(|| {
                    anyhow::anyhow!("no agg proof found for session_id: {}", session_id)
                })?;
                Ok(ProofArtifact::Aggregation(agg_proof.clone()))
            }
        }
    }
}
