//! `nitro-worker` library: leases Nitro TEE proof jobs from the `prover-service`, proves
//! them inside a running Nitro Enclave, and submits the signed attestations back.
//!
//! # Architecture
//!
//! ```text
//!  ┌──────────────────────────────────────────────────────────────────────────────────────┐
//!  │                     nitro-worker                                                     │
//!  │                                                                                      │
//!  │  poll prover_getNextProof(Nitro)  ← generic ProofWorker                             │
//!  │       │                                                                              │
//!  │       ▼                                                                              │
//!  │  build Kona witness over RPC (same path as bin/proof)                               │
//!  │       │                                                                              │
//!  │       ▼                                                                              │
//!  │  NitroProver::prove_range  ────────────► Nitro Enclave                              │
//!  │       │                                  (vsock / PCR-pinned)                       │
//!  │       ▼                                                                              │
//!  │  prover_submitProof(Nitro { attestation, public_values, signature })                │
//!  └──────────────────────────────────────────────────────────────────────────────────────┘
//! ```

#![cfg(target_os = "linux")]

use alloy_primitives::{B256, Bytes, keccak256};
use alloy_sol_types::SolValue;
use anyhow::{Context, Result, bail};
use tracing::{debug, info};
use world_chain_proof_kona_host::online::{
    OnlineHostConfig, RangeWitnessRequest, build_range_input,
};
use world_chain_proof_nitro_enclave::{
    ExpectedPcrs, NitroRangeProofRequest,
    host::{EnclaveEndpoint, NitroProver},
};
use world_chain_proof_protocol::ProofGameProvider;
use world_chain_proof_worker::{ClaimedProofJobHandler, ProofJob};
use world_chain_prover_service::{ProofBackend, ProofData};

// ──────────────────────────────────────────────────────────────────────────────────────
// NitroBackend — ClaimedProofJobHandler implementation for the Nitro TEE lane
// ──────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct NitroBackendConfig {
    pub online: OnlineHostConfig,
    pub enclave_cid: u32,
    pub enclave_port: u32,
    pub expected_pcrs: ExpectedPcrs,
}

pub struct NitroBackend<G> {
    config: NitroBackendConfig,
    game_provider: G,
}

impl<G> NitroBackend<G> {
    pub fn new(config: NitroBackendConfig, game_provider: G) -> Self {
        Self {
            config,
            game_provider,
        }
    }
}

#[async_trait::async_trait]
impl<G> ClaimedProofJobHandler for NitroBackend<G>
where
    G: ProofGameProvider,
{
    fn lane(&self) -> ProofBackend {
        ProofBackend::Nitro
    }

    fn verifier_id(&self) -> B256 {
        keccak256(self.config.expected_pcrs.pcr0)
    }

    async fn handle_claimed_job(&self, job: ProofJob) -> anyhow::Result<ProofData> {
        let request = &job.request;

        let game_context = self
            .game_provider
            .proof_game_context(request.game)
            .await
            .context("failed to read proof game context")?;
        let start_block = game_context
            .validated_start_block(
                request.game,
                request.root_claim,
                request.l2_block_number,
                request.l1_head,
                self.config.online.rollup_config_hash,
            )
            .context("proof request does not match its game")?;

        debug!(
            proof_id = %request.id(),
            game_address = %request.game,
            l2_block_number = request.l2_block_number,
            pre_state_block = start_block,
            block_interval = game_context.block_interval,
            worker_id = %job.worker_id,
            "validated Nitro proof range against game"
        );

        let endpoint =
            EnclaveEndpoint::with_port(self.config.enclave_cid, self.config.enclave_port);
        let prover = NitroProver::new(endpoint, self.config.expected_pcrs);

        info!(
            start_block,
            end_block = request.l2_block_number,
            l1_rpc = %self.config.online.l1_rpc,
            l2_rpc = %self.config.online.l2_rpc,
            "collecting witness data for range"
        );
        let witness_collection_started_at = std::time::Instant::now();
        let input = build_range_input(
            &self.config.online,
            RangeWitnessRequest {
                start_block,
                end_block: request.l2_block_number,
                l1_head: Some(request.l1_head),
                allow_unfinalized: false,
            },
        )
        .await
        .context("witness generation failed")?;

        let nitro_request = NitroRangeProofRequest::from_witness_data(&input.witness, None)
            .context("witness serialize")?;

        info!(
            start_block,
            end_block = request.l2_block_number,
            duration_secs = witness_collection_started_at.elapsed().as_secs_f64(),
            witness_bytes = nitro_request.witness_rkyv.len(),
            "witness data collection complete"
        );

        let artifact = prover
            .prove_range(nitro_request)
            .await
            .context("nitro enclave proving failed")?;

        if artifact.transition_public_values.l2PostRoot != request.root_claim {
            bail!(
                "enclave post root {:?} != claimed root {:?}",
                artifact.transition_public_values.l2PostRoot,
                request.root_claim
            );
        }
        if artifact.transition_public_values.l2PostBlockNumber != request.l2_block_number {
            bail!(
                "enclave block number {} != claimed {}",
                artifact.transition_public_values.l2PostBlockNumber,
                request.l2_block_number
            );
        }
        if artifact.transition_public_values.l1Head != request.l1_head {
            bail!(
                "enclave l1 head {:?} != claimed {:?}",
                artifact.transition_public_values.l1Head,
                request.l1_head
            );
        }
        if artifact.transition_public_values.rollupConfigHash
            != self.config.online.rollup_config_hash
        {
            bail!(
                "enclave rollup config hash {:?} != expected {:?}",
                artifact.transition_public_values.rollupConfigHash,
                self.config.online.rollup_config_hash
            );
        }

        info!(
            post_root = ?artifact.transition_public_values.l2PostRoot,
            block = artifact.transition_public_values.l2PostBlockNumber,
            l1_head = ?artifact.transition_public_values.l1Head,
            rollup_config_hash = ?artifact.transition_public_values.rollupConfigHash,
            "enclave attested range proof"
        );

        Ok(ProofData::Nitro {
            attestation: Bytes::from(artifact.attestation_doc),
            public_values: artifact.transition_public_values.abi_encode().into(),
            signature: Bytes::from(artifact.signature),
        })
    }
}
