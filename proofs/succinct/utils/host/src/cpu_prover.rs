//! Range proofs are produced in `Compressed` mode so the aggregation guest can recursively
//! verify them; the aggregation proof mode is configurable (Groth16 for on-chain verification).

use crate::{SuccinctProverError, WorldSuccinctProver};
use anyhow::Context;
use async_trait::async_trait;
pub use sp1_sdk::SP1ProofMode;
use sp1_sdk::{
    CpuProver, HashableKey, ProveRequest, Prover, ProvingKey, SP1Proof, SP1ProofWithPublicValues,
    SP1ProvingKey, SP1Stdin,
};
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use world_chain_proof_core::types::AggregationInputs;
use world_chain_proof_succinct_utils::{
    AggregationProofRequest, RangeProofRequest, Sp1ProofRequest, Sp1SessionStatus,
};

/// [`WorldSuccinctProver`] CPU-local implementation over the sp1-sdk local prover.
pub struct CpuSuccinctProver {
    client: CpuProver,
    range_pk: SP1ProvingKey,
    agg_pk: SP1ProvingKey,
    multi_block_vkey: [u32; 8],
    agg_mode: SP1ProofMode,

    next_session_id: AtomicU64,
    proofs: Mutex<HashMap<String, SP1ProofWithPublicValues>>,
}

impl CpuSuccinctProver {
    /// Creates the prover using caller-supplied ELFs. Use this in production binaries with
    /// ELFs embedded at compile time via `world_chain_proof_succinct_elfs`.
    pub async fn new(agg_mode: SP1ProofMode) -> anyhow::Result<Self> {
        let range_elf = world_chain_proof_succinct_elfs::range_elf();
        let agg_elf = world_chain_proof_succinct_elfs::aggregation_elf();
        let client = CpuProver::new().await;
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
            next_session_id: AtomicU64::new(0),
            proofs: Mutex::new(HashMap::default()),
        })
    }

    fn next_session_id(&self, prefix: &str) -> String {
        let id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{id}")
    }

    async fn prove_range(
        &self,
        request: RangeProofRequest,
    ) -> anyhow::Result<SP1ProofWithPublicValues> {
        let mut stdin = SP1Stdin::new();
        stdin.write_vec(request.witness_rkyv);

        let proof = self
            .client
            .prove(&self.range_pk, stdin)
            .compressed()
            .await
            .context("range proving failed")?;

        Ok(proof)
    }

    async fn prove_aggregation(
        &self,
        request: AggregationProofRequest,
    ) -> anyhow::Result<SP1ProofWithPublicValues> {
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
            .await
            .context("aggregation proving failed")?;

        self.client
            .verify(&proof, self.agg_pk.verifying_key(), None)
            .context("aggregation proof verification failed")?;

        Ok(proof)
    }
}

#[async_trait]
impl WorldSuccinctProver for CpuSuccinctProver {
    fn supports_persistent_sessions(&self) -> bool {
        false
    }

    async fn submit(&self, request: Sp1ProofRequest) -> anyhow::Result<String> {
        let (prefix, proof) = match request {
            Sp1ProofRequest::Range(range_request) => {
                ("range", self.prove_range(range_request).await?)
            }
            Sp1ProofRequest::Aggregation(session_request) => {
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
                ("aggregation", self.prove_aggregation(agg_request).await?)
            }
        };

        let session_id = self.next_session_id(prefix);
        let mut proofs = self
            .proofs
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex lock poisoned"))?;
        proofs.insert(session_id.clone(), proof);
        Ok(session_id)
    }

    async fn poll(&self, session_id: &str) -> anyhow::Result<Sp1SessionStatus> {
        let proofs = self
            .proofs
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex lock poisoned"))?;
        if proofs.contains_key(session_id) {
            // CPU prover immediately returns completed status if the entry exists in the hashmap.
            Ok(Sp1SessionStatus::Completed)
        } else {
            Ok(Sp1SessionStatus::NotFound)
        }
    }

    async fn download(&self, session_id: &str) -> anyhow::Result<SP1ProofWithPublicValues> {
        let proofs = self
            .proofs
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex lock poisoned"))?;
        let proof = proofs
            .get(session_id)
            .ok_or_else(|| anyhow::anyhow!("no proof found for session_id: {session_id}"))?;
        Ok(proof.clone())
    }
}
