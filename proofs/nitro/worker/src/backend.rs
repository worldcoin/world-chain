//! AWS Nitro TEE backend for the defender's [`ProofWorker`].
//!
//! The backend is the host-side counterpart to the `world-chain-nitro-enclave` binary:
//!
//! 1. It receives a [`ProofRequest`] from the `prover-service` over JSON-RPC.
//! 2. It builds the range witness over RPC using the shared Kona host pipeline.
//! 3. It hands the witness to a running Nitro Enclave through a vsock connection.
//! 4. It verifies the returned [`BootInfoStruct`] matches the requested claim and packages
//!    the raw attestation document + signature into a [`ProofData::Nitro`] value.
//!
//! Real attestation verification (PCR / `user_data` checks) is performed inside
//! [`NitroProver`] when `ExpectedPcrs` are configured; placeholder PCRs disable verification
//! and are intended for local devnet runs against a mock enclave.

use alloy_primitives::Bytes;
use anyhow::{Context, anyhow, bail};
use world_chain_proof_kona_host_utils::online::{
    OnlineHostConfig, RangeWitnessRequest, build_range_input,
};
use world_chain_proof_nitro::{
    ExpectedPcrs, NitroRangeProofRequest,
    host::{EnclaveEndpoint, NitroProver},
};
use world_chain_proof_worker::ProofJobBackend;
use world_chain_prover_service::{ProofBackend, ProofData, ProofRequest};

/// Configuration for [`NitroBackend`].
#[derive(Clone, Debug)]
pub struct NitroBackendConfig {
    /// L2 blocks between a proposal's parent and its claimed block (the proof system's
    /// `blockInterval` domain constant). The proved range is
    /// `(l2_block_number - block_interval, l2_block_number]`.
    pub block_interval: u64,
    /// RPC host config used to materialise range witnesses (same path as the SP1 worker).
    pub online: OnlineHostConfig,
    /// vsock CID the enclave is reachable on.
    pub enclave_cid: u32,
    /// vsock port the enclave listens on.
    pub enclave_port: u32,
    /// PCR values pinning the enclave image. Use [`ExpectedPcrs::PLACEHOLDER`] for local
    /// devnet runs where attestation verification should be skipped.
    pub expected_pcrs: ExpectedPcrs,
}

/// [`ProofJobBackend`] for the [`ProofBackend::Nitro`] lane: builds witnesses over RPC and
/// proves them inside a Nitro Enclave.
pub struct NitroBackend {
    config: NitroBackendConfig,
    /// Handle to the async runtime so we can call async enclave methods from the synchronous
    /// [`ProofJobBackend::prove`] entry point.
    rt_handle: tokio::runtime::Handle,
}

impl NitroBackend {
    /// Build a new backend.
    pub const fn new(config: NitroBackendConfig, rt_handle: tokio::runtime::Handle) -> Self {
        Self { config, rt_handle }
    }
}

impl ProofJobBackend for NitroBackend {
    fn lane(&self) -> ProofBackend {
        ProofBackend::Nitro
    }

    fn prove(&self, request: &ProofRequest) -> anyhow::Result<ProofData> {
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
        let prover =
            NitroProver::with_runtime(endpoint, self.config.expected_pcrs, self.rt_handle.clone());

        let input = build_range_input(
            &self.config.online,
            RangeWitnessRequest {
                start_block,
                end_block: request.l2_block_number,
                l1_head: Some(request.l1_head),
                allow_unfinalized: false,
            },
        )
        .context("witness generation failed")?;

        let nitro_request = NitroRangeProofRequest::from_witness_data(&input.witness, None)
            .context("witness serialize")?;

        let artifact = self
            .rt_handle
            .block_on(prover.prove_range_async(nitro_request))
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

        tracing::info!(
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
