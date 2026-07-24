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

use alloy_primitives::{B256, Bytes};
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
    /// sanity check performed before dispatching a job to the enclave.
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

        // Fast-fail: check the claimed post-state output root against the L2 rollup node's
        // authoritative view *before* dispatching expensive witness generation to the
        // enclave. Without this, a request with an `l2_block_number` that hasn't been
        // produced yet (or a `root_claim` that simply doesn't match the real chain) still
        // runs the full witness-generation pipeline and only fails once the enclave returns,
        // up to `witness_timeout` (900s by default) later. Rejecting here turns that into an
        // immediate error.
        validate_output_root(
            &OptimismConsensusClient::new(self.config.output_root_rpc.clone()),
            request.l2_block_number,
            request.root_claim,
        )
        .await?;

        let endpoint =
            EnclaveEndpoint::with_port(self.config.enclave_cid, self.config.enclave_port);
        let prover = NitroProver::new(endpoint, self.config.expected_pcrs);

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

/// Checks the request's claimed post-state output root against the real output root the
/// L2 rollup node reports for `l2_block_number`, without touching the enclave.
///
/// Returns an error immediately when:
/// - `l2_block_number` doesn't correspond to a real, known L2 block (the rollup node's
///   `optimism_outputAtBlock` call errors or has nothing to report), or
/// - the rollup node's real output root at that block differs from `root_claim`.
///
/// This is deliberately generic over [`ConsensusProvider`] so the comparison logic can be
/// unit tested without a live RPC endpoint.
async fn validate_output_root<C: ConsensusProvider>(
    consensus: &C,
    l2_block_number: u64,
    root_claim: B256,
) -> Result<()> {
    let real_root = consensus
        .output_root_at_block(l2_block_number)
        .await
        .map_err(|error: ConsensusError| {
            anyhow!(
                "fast-fail validation failed: l2_block_number {l2_block_number} does not \
                 correspond to a known L2 block (optimism_outputAtBlock error: {error})"
            )
        })?;

    if real_root != root_claim {
        bail!(
            "fast-fail validation failed: claimed root_claim {root_claim:?} does not match \
             the real output root {real_root:?} at L2 block {l2_block_number}; refusing to \
             start enclave witness generation"
        );
    }

    Ok(())
}

#[cfg(test)]
mod fast_fail_tests {
    use super::{ConsensusError, ConsensusProvider, validate_output_root};
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
            unimplemented!("not used by validate_output_root")
        }
    }

    #[tokio::test]
    async fn accepts_matching_root() {
        let root = B256::repeat_byte(0x42);
        let provider = FixedConsensusProvider {
            output_root: Ok(root),
        };
        validate_output_root(&provider, 100, root)
            .await
            .expect("matching root should validate");
    }

    #[tokio::test]
    async fn rejects_mismatched_root() {
        let provider = FixedConsensusProvider {
            output_root: Ok(B256::repeat_byte(0x01)),
        };
        let error = validate_output_root(&provider, 100, B256::repeat_byte(0x02))
            .await
            .expect_err("mismatched root must fail fast");
        assert!(error.to_string().contains("does not match"));
    }

    #[tokio::test]
    async fn rejects_unknown_block() {
        let provider = FixedConsensusProvider {
            output_root: Err("block not found".to_string()),
        };
        let error = validate_output_root(&provider, 100, B256::repeat_byte(0x02))
            .await
            .expect_err("unknown block must fail fast");
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
