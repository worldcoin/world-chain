#![cfg(target_os = "linux")]

use std::{path::PathBuf, sync::Arc, time::Duration};

use alloy_primitives::B256;
use anyhow::{Context, Result};
use clap::Parser;
use tracing::info;
use world_chain_chainspec::WorldChainSpec;
use world_chain_nitro_worker::{NitroBackend, NitroBackendConfig, build_expected_pcrs};
use world_chain_proof_kona_host_utils::online::{
    build_online_config, hardfork_config_from_chain_spec,
};
use world_chain_proof_worker::{
    ProofWorker, ProofWorkerConfig, RetryConfig, WorkerHeartbeatConfig,
};
use world_chain_prover_service::RpcProverServiceClient;

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

use crate::cmd::common::CommonArgs;

#[derive(Debug, Parser)]
#[command(
    about = "World Chain Nitro TEE proving worker: leases jobs from the prover-service, \
             proves them in a Nitro Enclave, and submits the signed attestations back."
)]
pub struct WorkerArgs {
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
    #[arg(
        long,
        env = "ENCLAVE_PORT",
        default_value_t = world_chain_proof_nitro::protocol::DEFAULT_VSOCK_PORT
    )]
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

    /// The worker heartbeat interval in seconds.
    #[arg(long, default_value_t = DEFAULT_WORKER_HEARTBEAT_INTERVAL_SEC)]
    heartbeat_interval_sec: u64,

    /// Maximum consecutive retryable heartbeat failures before aborting proof generation.
    #[arg(long, default_value_t = DEFAULT_WORKER_MAX_CONSECUTIVE_HEARTBEAT_FAILURES)]
    heartbeat_max_consecutive_failures: u32,
}

pub async fn run(args: WorkerArgs) -> Result<()> {
    let spec = args.network.chain_spec();
    let schedule = hardfork_config_from_chain_spec(spec.as_ref());
    let online = build_online_config(
        args.rollup_config.clone(),
        args.rollup_config_hash,
        args.l1_rpc.clone(),
        args.l1_beacon_rpc.clone(),
        args.l2_rpc.clone(),
        args.network.chain_id(),
        &schedule,
        Duration::from_secs(args.witness_timeout_seconds),
    )?;
    let expected_pcrs = build_expected_pcrs(
        args.pcr0.as_deref(),
        args.pcr1.as_deref(),
        args.pcr2.as_deref(),
    )?;

    info!(
        prover_service = %args.prover_service_url,
        enclave_cid = args.enclave_cid,
        block_interval = args.block_interval,
        submit_proof_retry_max_retries = args.submit_proof_retry_max_retries,
        submit_proof_retry_initial_delay_ms = args.submit_proof_retry_initial_delay_ms,
        submit_proof_retry_max_delay_ms = args.submit_proof_retry_max_delay_ms,
        "nitro-worker starting"
    );

    let backend = NitroBackend::new(NitroBackendConfig {
        block_interval: args.block_interval,
        online,
        enclave_cid: args.enclave_cid,
        enclave_port: args.enclave_port,
        expected_pcrs,
    });

    let queue = RpcProverServiceClient::new(&args.prover_service_url)
        .with_context(|| format!("failed to connect to {}", args.prover_service_url))?;

    let worker_id = format!("{}-nitro-worker", args.worker_id);
    let retry_config = RetryConfig::new(
        args.submit_proof_retry_max_retries,
        Duration::from_millis(args.submit_proof_retry_initial_delay_ms),
        Duration::from_millis(args.submit_proof_retry_max_delay_ms),
    );
    let heartbeat_config = WorkerHeartbeatConfig::with_max_consecutive_failures(
        Duration::from_secs(args.heartbeat_interval_sec),
        args.heartbeat_max_consecutive_failures,
    );
    let worker = ProofWorker::new(
        queue,
        backend,
        ProofWorkerConfig {
            worker_id,
            poll_interval: Duration::from_secs(args.poll_interval_seconds),
            max_concurrent_jobs: args.max_concurrent_jobs,
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
