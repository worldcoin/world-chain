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
//!  │  prover_submitProof(Nitro { attestation, signature })                               │
//!  └──────────────────────────────────────────────────────────────────────────────────────┘
//! ```

#![cfg(target_os = "linux")]

use alloy_primitives::Bytes;
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

        if artifact.boot_info.l2PostRoot != request.root_claim {
            bail!(
                "enclave post root {:?} != claimed root {:?}",
                artifact.boot_info.l2PostRoot,
                request.root_claim
            );
        }
        if artifact.boot_info.l2BlockNumber != request.l2_block_number {
            bail!(
                "enclave block number {} != claimed {}",
                artifact.boot_info.l2BlockNumber,
                request.l2_block_number
            );
        }
        if artifact.boot_info.l1Head != request.l1_head {
            bail!(
                "enclave l1 head {:?} != claimed {:?}",
                artifact.boot_info.l1Head,
                request.l1_head
            );
        }
        if artifact.boot_info.rollupConfigHash != self.config.online.rollup_config_hash {
            bail!(
                "enclave rollup config hash {:?} != expected {:?}",
                artifact.boot_info.rollupConfigHash,
                self.config.online.rollup_config_hash
            );
        }

        info!(
            post_root = ?artifact.boot_info.l2PostRoot,
            block = artifact.boot_info.l2BlockNumber,
            l1_head = ?artifact.boot_info.l1Head,
            rollup_config_hash = ?artifact.boot_info.rollupConfigHash,
            "enclave attested range proof"
        );

        Ok(ProofData::Nitro {
            attestation: Bytes::from(artifact.attestation_doc),
            signature: Bytes::from(artifact.signature),
        })
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
