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
//!  │  NitroProver::prove_range_async  ──────► Nitro Enclave                              │
//!  │       │                                  (vsock / PCR-pinned)                       │
//!  │       ▼                                                                              │
//!  │  prover_submitProof(Nitro { attestation, signature })                               │
//!  └──────────────────────────────────────────────────────────────────────────────────────┘
//! ```

#![cfg(target_os = "linux")]

use std::{path::PathBuf, sync::Arc, time::Duration};

use alloy_primitives::{B256, Bytes};
use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use tracing::{info, warn};
use world_chain_chainspec::WorldChainSpec;
use world_chain_proof_kona_host_utils::online::{
    OnlineHostConfig, RangeWitnessRequest, build_online_config, build_range_input,
    hardfork_config_from_chain_spec,
};
use world_chain_proof_nitro::{
    ExpectedPcrs, NitroRangeProofRequest,
    host::{EnclaveEndpoint, NitroProver},
};
use world_chain_proof_worker::{
    ClaimedProofJobHandler, ProofJob, ProofWorker, ProofWorkerConfig, RetryConfig,
    WorkerHeartbeatConfig,
};
use world_chain_prover_service::{ProofBackend, ProofData, RpcProverServiceClient};

const DEFAULT_SUBMIT_PROOF_RETRY_MAX_RETRIES: usize = 10;
const DEFAULT_SUBMIT_PROOF_RETRY_INITIAL_DELAY_MS: u64 = 100;
const DEFAULT_SUBMIT_PROOF_RETRY_MAX_DELAY_MS: u64 = 10_000;
const DEFAULT_WORKER_HEARTBEAT_INTERVAL_SEC: u64 = 30;
const DEFAULT_WORKER_MAX_CONSECUTIVE_HEARTBEAT_FAILURES: u32 = 5;

// ──────────────────────────────────────────────────────────────────────────────────────
// NitroBackend — ClaimedProofJobHandler implementation for the Nitro TEE lane
// ──────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct NitroBackendConfig {
    block_interval: u64,
    online: OnlineHostConfig,
    enclave_cid: u32,
    enclave_port: u32,
    expected_pcrs: ExpectedPcrs,
}

struct NitroBackend {
    config: NitroBackendConfig,
}

impl NitroBackend {
    fn new(config: NitroBackendConfig) -> Self {
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
            .prove_range_async(nitro_request)
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
// CLI / config
// ──────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Network {
    #[value(name = "worldchain")]
    WorldChain,
    #[value(name = "worldchain-sepolia")]
    WorldChainSepolia,
}

impl Network {
    fn chain_id(self) -> u64 {
        match self {
            Self::WorldChain => 480,
            Self::WorldChainSepolia => 4801,
        }
    }

    fn chain_spec(self) -> Arc<WorldChainSpec> {
        match self {
            Self::WorldChain => WorldChainSpec::mainnet(),
            Self::WorldChainSepolia => WorldChainSpec::sepolia(),
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "nitro-worker",
    about = "World Chain Nitro TEE proving worker: leases jobs from the prover-service, \
             proves them in a Nitro Enclave, and submits the signed attestations back."
)]
struct Cli {
    /// prover-service JSON-RPC URL.
    #[arg(long, env = "PROVER_SERVICE_URL")]
    prover_service_url: String,

    /// World Chain L2 execution RPC URL.
    #[arg(long, env = "L2_RPC_URL")]
    l2_rpc: String,

    /// Ethereum L1 execution RPC URL.
    #[arg(long, env = "L1_RPC_URL")]
    l1_rpc: String,

    /// Ethereum L1 beacon API URL.
    #[arg(long, env = "L1_BEACON_RPC_URL")]
    l1_beacon_rpc: String,

    /// World Chain network.
    #[arg(long, env = "NETWORK", default_value = "worldchain")]
    network: Network,

    /// Rollup config JSON file. If omitted, uses the built-in network config.
    #[arg(long, env = "ROLLUP_CONFIG")]
    rollup_config: Option<PathBuf>,

    /// Rollup config hash override (required when --rollup-config is not supplied).
    #[arg(long, env = "ROLLUP_CONFIG_HASH")]
    rollup_config_hash: Option<B256>,

    /// L2 blocks between a proposal's parent and its claimed block (the proof system's
    /// `blockInterval` domain constant).
    #[arg(long, env = "BLOCK_INTERVAL")]
    block_interval: u64,

    /// vsock CID of the running Nitro Enclave.
    #[arg(long, env = "ENCLAVE_CID", default_value_t = 16)]
    enclave_cid: u32,

    /// vsock port the enclave listens on.
    #[arg(long, env = "ENCLAVE_PORT", default_value_t = world_chain_proof_nitro::protocol::DEFAULT_VSOCK_PORT)]
    enclave_port: u32,

    /// PCR0 hex (48 bytes). All three PCRs must be provided for production use.
    #[arg(long, env = "PCR0")]
    pcr0: Option<String>,

    /// PCR1 hex (48 bytes).
    #[arg(long, env = "PCR1")]
    pcr1: Option<String>,

    /// PCR2 hex (48 bytes).
    #[arg(long, env = "PCR2")]
    pcr2: Option<String>,

    /// Seconds to sleep between job-queue polls when no work is available.
    #[arg(long, env = "POLL_INTERVAL_SECONDS", default_value_t = 10)]
    poll_interval_seconds: u64,

    /// Maximum seconds to spend generating one Kona witness.
    #[arg(long, default_value_t = 900)]
    witness_timeout_seconds: u64,

    /// Maximum number of jobs proved concurrently. TEE attestation is cheaper than ZK
    /// proving, so this can be higher than for SP1 workers.
    #[arg(long, default_value_t = 1)]
    max_concurrent_jobs: usize,

    /// Maximum retries after a retryable submitProof failure.
    #[arg(
        long,
        env = "SUBMIT_PROOF_RETRY_MAX_RETRIES",
        default_value_t = DEFAULT_SUBMIT_PROOF_RETRY_MAX_RETRIES
    )]
    submit_proof_retry_max_retries: usize,

    /// Initial delay in milliseconds before retrying submitProof.
    #[arg(
        long,
        env = "SUBMIT_PROOF_RETRY_INITIAL_DELAY_MS",
        default_value_t = DEFAULT_SUBMIT_PROOF_RETRY_INITIAL_DELAY_MS
    )]
    submit_proof_retry_initial_delay_ms: u64,

    /// Maximum delay in milliseconds between submitProof retries.
    #[arg(
        long,
        env = "SUBMIT_PROOF_RETRY_MAX_DELAY_MS",
        default_value_t = DEFAULT_SUBMIT_PROOF_RETRY_MAX_DELAY_MS
    )]
    submit_proof_retry_max_delay_ms: u64,

    /// The unique worker id.
    #[arg(long)]
    worker_id: String,

    #[arg(long, default_value_t = DEFAULT_WORKER_HEARTBEAT_INTERVAL_SEC)]
    /// The worker heartbeat interval in seconds.
    heartbeat_interval_sec: u64,

    #[arg(long, default_value_t = DEFAULT_WORKER_MAX_CONSECUTIVE_HEARTBEAT_FAILURES)]
    /// Maximum consecutive retryable heartbeat failures before aborting proof generation.
    heartbeat_max_consecutive_failures: u32,
}

// ──────────────────────────────────────────────────────────────────────────────────────
// Entry point
// ──────────────────────────────────────────────────────────────────────────────────────

pub async fn run() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let spec = cli.network.chain_spec();
    let schedule = hardfork_config_from_chain_spec(spec.as_ref());
    let online = build_online_config(
        cli.rollup_config.clone(),
        cli.rollup_config_hash,
        cli.l1_rpc.clone(),
        cli.l1_beacon_rpc.clone(),
        cli.l2_rpc.clone(),
        cli.network.chain_id(),
        &schedule,
        Duration::from_secs(cli.witness_timeout_seconds),
    )?;
    let expected_pcrs = build_expected_pcrs(
        cli.pcr0.as_deref(),
        cli.pcr1.as_deref(),
        cli.pcr2.as_deref(),
    )?;

    info!(
        prover_service = %cli.prover_service_url,
        enclave_cid = cli.enclave_cid,
        block_interval = cli.block_interval,
        submit_proof_retry_max_retries = cli.submit_proof_retry_max_retries,
        submit_proof_retry_initial_delay_ms = cli.submit_proof_retry_initial_delay_ms,
        submit_proof_retry_max_delay_ms = cli.submit_proof_retry_max_delay_ms,
        "nitro-worker starting"
    );

    let backend_config = NitroBackendConfig {
        block_interval: cli.block_interval,
        online,
        enclave_cid: cli.enclave_cid,
        enclave_port: cli.enclave_port,
        expected_pcrs,
    };

    let backend = NitroBackend::new(backend_config);

    let queue = RpcProverServiceClient::new(&cli.prover_service_url)
        .with_context(|| format!("failed to connect to {}", cli.prover_service_url))?;

    let worker_id = format!("{}-nitro-worker", cli.worker_id);
    let retry_initial_delay = Duration::from_millis(cli.submit_proof_retry_initial_delay_ms);
    let retry_max_delay = Duration::from_millis(cli.submit_proof_retry_max_delay_ms);
    let retry_config = RetryConfig::new(
        cli.submit_proof_retry_max_retries,
        retry_initial_delay,
        retry_max_delay,
    );
    let heartbeat_config = WorkerHeartbeatConfig::with_max_consecutive_failures(
        Duration::from_secs(cli.heartbeat_interval_sec),
        cli.heartbeat_max_consecutive_failures,
    );
    let worker = ProofWorker::new(
        queue,
        backend,
        ProofWorkerConfig {
            worker_id,
            poll_interval: Duration::from_secs(cli.poll_interval_seconds),
            max_concurrent_jobs: cli.max_concurrent_jobs,
            retry_config,
            heartbeat_config,
        },
    );

    // Ctrl-C triggers a graceful shutdown: the worker stops leasing, signals the backend to
    // shut down, and resolves.
    let token = worker.cancellation_token();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            info!("received ctrl-c, shutting down");
            token.cancel();
        }
    });

    worker.await;
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────────────
// Config helpers
// ──────────────────────────────────────────────────────────────────────────────────────

fn build_expected_pcrs(
    pcr0: Option<&str>,
    pcr1: Option<&str>,
    pcr2: Option<&str>,
) -> Result<ExpectedPcrs> {
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

fn hex_to_pcr(s: &str) -> Result<[u8; world_chain_proof_nitro::PCR_LEN]> {
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
