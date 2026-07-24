//! `nitro-worker` library: leases Nitro TEE proof jobs from the `prover-service`, proves
//! them inside a running Nitro Enclave, and submits the signed attestations back.
//!
//! Before dispatching a claimed job, the worker fast-fails requests whose pre-state
//! checkpoint isn't a real L2 block (see `validate_pre_state_block`). It does **not**
//! fast-fail on `root_claim` (the post-state, disputed claim) disagreeing with any
//! node's opinion — that's exactly the case fault proofs exist to resolve, and it
//! must always go through full derivation via the enclave.
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

use alloy_primitives::Bytes;
use alloy_sol_types::SolValue;
use anyhow::{Context, Result, anyhow, bail};
use tracing::info;
use world_chain_proof_kona_host_utils::online::{
    OnlineHostConfig, RangeWitnessRequest, build_range_input,
};
use world_chain_proof_nitro::{
    ExpectedPcrs, NitroRangeProofRequest,
    host::{EnclaveEndpoint, NitroProver},
};
use world_chain_proof_worker::{ClaimedProofJobHandler, ProofJob};
use world_chain_proofs::{ConsensusError, ConsensusProvider, OptimismConsensusClient};
use world_chain_prover_service::{ProofBackend, ProofData};

// ──────────────────────────────────────────────────────────────────────────────────────
// NitroBackend — ClaimedProofJobHandler implementation for the Nitro TEE lane
// ──────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct NitroBackendConfig {
    pub block_interval: u64,
    pub online: OnlineHostConfig,
    pub enclave_cid: u32,
    pub enclave_port: u32,
    pub expected_pcrs: ExpectedPcrs,
    /// op-node (rollup) JSON-RPC URL used for the fast-fail `optimism_outputAtBlock`
    /// sanity check of the request's pre-state block, performed before dispatching a job
    /// to the enclave.
    pub output_root_rpc: String,
}

pub struct NitroBackend {
    config: NitroBackendConfig,
}

impl NitroBackend {
    pub fn new(config: NitroBackendConfig) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl ClaimedProofJobHandler for NitroBackend {
    fn lane(&self) -> ProofBackend {
        ProofBackend::Nitro
    }

    async fn handle_claimed_job(&self, job: ProofJob) -> anyhow::Result<ProofData> {
        let request = &job.request;

        let start_block = request
            .l2_block_number
            .checked_sub(self.config.block_interval)
            .ok_or_else(|| {
                anyhow!(
                    "l2_block_number {} is below block_interval {}",
                    request.l2_block_number,
                    self.config.block_interval
                )
            })?;

        info!(
            job_id = %request.id(),
            game = %request.game,
            l2_block_number = request.l2_block_number,
            pre_state_block = start_block,
            worker_id = %job.worker_id,
            "nitro worker claimed proof job"
        );

        // Fast-fail: confirm the request's pre-state block (`start_block`) is a real,
        // known L2 block before dispatching expensive witness generation to the enclave.
        //
        // This deliberately does NOT touch `root_claim` (the post-state, disputed claim).
        // The entire point of the Nitro worker is to independently re-derive that claim
        // via the enclave and let on-chain verification decide whether it's correct — a
        // request whose `root_claim` disagrees with this (or any single) node's opinion is
        // exactly the legitimate dispute scenario fault proofs exist to resolve, so it must
        // still go through full derivation. Only the *pre-state* is expected to already be
        // an agreed-upon, finalized safe checkpoint both parties recognize; if it doesn't
        // correspond to a real L2 block at all, the request is malformed input (for example
        // a typo'd or not-yet-produced block number), not a dispute, and we can reject it
        // immediately instead of burning up to `witness_timeout` (900s by default) finding
        // that out the hard way.
        validate_pre_state_block(
            &OptimismConsensusClient::new(self.config.output_root_rpc.clone()),
            start_block,
        )
        .await?;

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

// ──────────────────────────────────────────────────────────────────────────────────────
// Fast-fail validation
// ──────────────────────────────────────────────────────────────────────────────────────

/// Confirms the request's pre-state checkpoint (`start_block` = `l2_block_number -
/// block_interval`) is a real, known L2 block the rollup node recognizes, without
/// touching the enclave.
///
/// Returns an error immediately when `pre_state_block` doesn't correspond to a real,
/// known L2 block (the rollup node's `optimism_outputAtBlock` call errors or has nothing
/// to report) — for example because the request carries a malformed or not-yet-produced
/// block number.
///
/// Deliberately does **not** validate `root_claim` (the post-state, disputed claim)
/// against this or any other node's opinion: the whole point of the Nitro worker is to
/// independently re-derive that claim via the enclave and let on-chain verification
/// decide if it's correct. A `root_claim` that disagrees with a single node's view is not
/// necessarily wrong — it may be exactly the legitimate dispute fault proofs exist to
/// resolve — so it must always go through full derivation regardless of whether it
/// matches.
///
/// This is deliberately generic over [`ConsensusProvider`] so the check can be unit
/// tested without a live RPC endpoint.
async fn validate_pre_state_block<C: ConsensusProvider>(
    consensus: &C,
    pre_state_block: u64,
) -> Result<()> {
    consensus
        .output_root_at_block(pre_state_block)
        .await
        .map_err(|error: ConsensusError| {
            anyhow!(
                "fast-fail validation failed: pre-state block {pre_state_block} does not \
                 correspond to a known L2 block (optimism_outputAtBlock error: {error})"
            )
        })?;

    Ok(())
}

#[cfg(test)]
mod fast_fail_tests {
    use super::{ConsensusError, ConsensusProvider, validate_pre_state_block};
    use alloy_primitives::{B256, BlockNumber};

    struct FixedConsensusProvider {
        output_root: Result<B256, String>,
    }

    #[async_trait::async_trait]
    impl ConsensusProvider for FixedConsensusProvider {
        async fn output_root_at_block(
            &self,
            _l2_block_number: u64,
        ) -> Result<B256, ConsensusError> {
            self.output_root.clone().map_err(ConsensusError::Rpc)
        }

        async fn latest_l2_finalized_block(&self) -> Result<BlockNumber, ConsensusError> {
            unimplemented!("not used by validate_pre_state_block")
        }
    }

    #[tokio::test]
    async fn accepts_known_pre_state_block() {
        let provider = FixedConsensusProvider {
            output_root: Ok(B256::repeat_byte(0x42)),
        };
        validate_pre_state_block(&provider, 100)
            .await
            .expect("known pre-state block should validate");
    }

    #[tokio::test]
    async fn rejects_unknown_pre_state_block() {
        let provider = FixedConsensusProvider {
            output_root: Err("block not found".to_string()),
        };
        let error = validate_pre_state_block(&provider, 100)
            .await
            .expect_err("unknown pre-state block must fail fast");
        assert!(
            error
                .to_string()
                .contains("does not correspond to a known L2 block")
        );
    }
}

// ──────────────────────────────────────────────────────────────────────────────────────
// PCR helpers (used by the binary to validate CLI inputs)
// ──────────────────────────────────────────────────────────────────────────────────────

pub fn build_expected_pcrs(
    pcr0: Option<&str>,
    pcr1: Option<&str>,
    pcr2: Option<&str>,
) -> Result<ExpectedPcrs> {
    use tracing::warn;
    match (pcr0, pcr1, pcr2) {
        (Some(p0), Some(p1), Some(p2)) => Ok(ExpectedPcrs {
            pcr0: hex_to_pcr(p0)?,
            pcr1: hex_to_pcr(p1)?,
            pcr2: hex_to_pcr(p2)?,
        }),
        (None, None, None) => {
            warn!(
                "PCRs not configured; using placeholder zeros. \
                 Production REQUIRES --pcr0/--pcr1/--pcr2."
            );
            Ok(ExpectedPcrs::PLACEHOLDER)
        }
        _ => bail!("provide all three of --pcr0/--pcr1/--pcr2, or none"),
    }
}

pub fn hex_to_pcr(s: &str) -> Result<[u8; world_chain_proof_nitro::PCR_LEN]> {
    let bytes =
        hex::decode(s.trim_start_matches("0x")).with_context(|| format!("invalid PCR hex: {s}"))?;
    if bytes.len() != world_chain_proof_nitro::PCR_LEN {
        bail!(
            "PCR must be {} bytes, got {}",
            world_chain_proof_nitro::PCR_LEN,
            bytes.len()
        );
    }
    let mut arr = [0u8; world_chain_proof_nitro::PCR_LEN];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}
