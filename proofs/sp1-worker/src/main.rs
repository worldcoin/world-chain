//! `sp1-worker` binary: leases SP1 proof jobs from the `prover-service`, proves them, and
//! submits the proofs back.

use alloy_primitives::{Address, B256};
use anyhow::{Context, Result};
use clap::Parser;
use std::{path::PathBuf, sync::Arc, time::Duration};
use world_chain_chainspec::WorldChainSpec;
use world_chain_proof_kona_host_utils::online::build_online_config;
use world_chain_proof_protocol::WorldHardforkConfig as ProtocolHardforkConfig;
use world_chain_proof_succinct_host_utils::{
    Sp1ProverKind,
    cpu_prover::{CpuSuccinctProver, SP1ProofMode},
};
use world_chain_proof_worker::WorkerHeartbeatConfig;
use world_chain_prover_service::RpcProverServiceClient;
use world_chain_sp1_worker::{
    ProofWorker, ProofWorkerConfig, RetryConfig, Sp1Backend, Sp1BackendConfig,
};

const DEFAULT_SUBMIT_PROOF_RETRY_MAX_RETRIES: usize = 10;
const DEFAULT_SUBMIT_PROOF_RETRY_INITIAL_DELAY_MS: u64 = 100;
const DEFAULT_SUBMIT_PROOF_RETRY_MAX_DELAY_MS: u64 = 10_000;
const DEFAULT_WORKER_HEARTBEAT_INTERVAL_SEC: u64 = 30;
const DEFAULT_WORKER_MAX_CONSECUTIVE_HEARTBEAT_FAILURES: u32 = 5;

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
    name = "sp1-worker",
    about = "World Chain SP1 proving worker: leases jobs from the prover-service, proves them, and submits the proofs back"
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

    /// World Chain network to prove.
    #[arg(long, env = "NETWORK", default_value = "worldchain")]
    network: Network,

    /// Rollup config JSON file. If omitted, uses the built-in network config.
    #[arg(long, env = "ROLLUP_CONFIG")]
    rollup_config: Option<PathBuf>,

    /// Rollup config hash override (required when --rollup-config is not supplied).
    #[arg(long, env = "ROLLUP_CONFIG_HASH")]
    rollup_config_hash: Option<B256>,

    /// L2 blocks between a proposal's parent and its claimed block (the proof system's
    /// blockInterval domain constant).
    #[arg(long, env = "BLOCK_INTERVAL")]
    block_interval: u64,

    /// Number of equal-length sub-ranges proved independently per job.
    #[arg(long, default_value_t = 1)]
    ranges: u64,

    /// Allow proving blocks newer than the finalized L2 head.
    #[arg(long)]
    allow_unfinalized: bool,

    /// Maximum seconds to spend generating one Kona witness.
    #[arg(long, default_value_t = 900)]
    witness_timeout_seconds: u64,

    /// Prover backend. Mock and network are reserved for follow-up implementations.
    #[arg(
        long,
        env = "SP1_PROVER",
        default_value_t = Sp1ProverKind::Cpu
    )]
    prover: Sp1ProverKind,

    /// Prover address for on-chain attribution (defaults to zero address).
    #[arg(
        long,
        env = "PROVER_ADDRESS",
        default_value = "0x0000000000000000000000000000000000000000"
    )]
    prover_address: Address,

    /// Seconds to sleep between job-queue polls when no work is available.
    #[arg(long, default_value_t = 10)]
    poll_interval_seconds: u64,

    /// Maximum number of jobs proved concurrently. One suits a local CPU prover; raise it for
    /// the Succinct proving network.
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

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let spec = cli.network.chain_spec();
    let protocol_cfg = ProtocolHardforkConfig::from_chain_spec(spec.as_ref());
    let host = build_online_config(
        cli.rollup_config.clone(),
        cli.rollup_config_hash,
        cli.l1_rpc.clone(),
        cli.l1_beacon_rpc.clone(),
        cli.l2_rpc.clone(),
        cli.network.chain_id(),
        &protocol_cfg,
        Duration::from_secs(cli.witness_timeout_seconds),
    )?;

    // ELFs are embedded at compile time via `sp1_sdk::include_elf!()`
    // (see `proofs/succinct/elfs/build.rs`). Challenged roots are
    // defended on-chain; Groth16 keeps verification ~100k gas.
    let prover = match cli.prover {
        Sp1ProverKind::Cpu => CpuSuccinctProver::new(SP1ProofMode::Groth16).await?,
        Sp1ProverKind::Mock | Sp1ProverKind::Network => {
            anyhow::bail!(
                "unsupported SP1 prover '{}'; only 'cpu' is currently available",
                cli.prover
            );
        }
    };

    let backend = Sp1Backend::new(
        host,
        prover,
        Sp1BackendConfig {
            block_interval: cli.block_interval,
            split_count: cli.ranges.max(1),
            prover_address: cli.prover_address,
            allow_unfinalized: cli.allow_unfinalized,
        },
    );

    let queue = RpcProverServiceClient::new(&cli.prover_service_url)
        .with_context(|| format!("failed to connect to {}", cli.prover_service_url))?;
    let worker_id = format!("{}-sp1-worker", cli.worker_id);
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

    tracing::info!(
        prover_service = %cli.prover_service_url,
        block_interval = cli.block_interval,
        ranges = cli.ranges.max(1),
        prover = %cli.prover,
        submit_proof_retry_max_retries = cli.submit_proof_retry_max_retries,
        submit_proof_retry_initial_delay_ms = cli.submit_proof_retry_initial_delay_ms,
        submit_proof_retry_max_delay_ms = cli.submit_proof_retry_max_delay_ms,
        "sp1-worker starting"
    );

    // Ctrl-C triggers a graceful shutdown: the worker stops leasing, flushes pending
    // reports, and resolves.
    let token = worker.cancellation_token();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("received ctrl-c, shutting down");
            token.cancel();
        }
    });

    worker.await;
    Ok(())
}
