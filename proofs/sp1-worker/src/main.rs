//! `sp1-worker` binary: leases SP1 proof jobs from the `prover-service`, proves them, and
//! submits the proofs back.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use alloy_primitives::{Address, B256};
use anyhow::{Context, Result};
use clap::Parser;
use serde_json::Value;
use world_chain_chainspec::WorldChainSpec;
use world_chain_proof_core::range::WorldRangeHardforkConfig;
use world_chain_proof_protocol::WorldHardforkConfig as ProtocolHardforkConfig;
use world_chain_proof_succinct_host_utils::{
    env_prover::{EnvSuccinctProver, SP1ProofMode, Sp1ProverKind},
    online::OnlineHostConfig,
};
use world_chain_prover_service::RpcProverServiceClient;
use world_chain_sp1_worker::{Sp1Backend, Sp1BackendConfig, Sp1Worker};

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

    /// Path to the SP1 range ELF binary.
    #[arg(long, env = "RANGE_ELF_PATH")]
    range_elf: PathBuf,

    /// Path to the SP1 aggregation ELF binary.
    #[arg(long, env = "AGG_ELF_PATH")]
    agg_elf: PathBuf,

    /// Prover backend: cpu, mock, or network. Overrides SP1_PROVER env var.
    #[arg(long, env = "SP1_PROVER", default_value = "cpu")]
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
}

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let (schedule, rollup_config_hash) = proof_config(
        cli.network,
        cli.rollup_config.as_deref(),
        cli.rollup_config_hash,
    )?;
    let host = OnlineHostConfig {
        l1_rpc: cli.l1_rpc,
        l1_beacon_rpc: cli.l1_beacon_rpc,
        l2_rpc: cli.l2_rpc,
        schedule,
        rollup_config_hash,
        l2_chain_id: cli
            .rollup_config
            .is_none()
            .then_some(cli.network.chain_id()),
        rollup_config_path: cli.rollup_config,
        witness_timeout: Duration::from_secs(cli.witness_timeout_seconds),
    };

    let range_elf = fs::read(&cli.range_elf)
        .with_context(|| format!("failed to read {}", cli.range_elf.display()))?;
    let agg_elf = fs::read(&cli.agg_elf)
        .with_context(|| format!("failed to read {}", cli.agg_elf.display()))?;
    // Challenged roots are defended on-chain; Groth16 keeps verification ~100k gas.
    let prover = EnvSuccinctProver::new(cli.prover, range_elf, agg_elf, SP1ProofMode::Groth16)?;

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
    let worker = Sp1Worker::new(
        queue,
        backend,
        Duration::from_secs(cli.poll_interval_seconds),
    );

    tracing::info!(
        prover_service = %cli.prover_service_url,
        block_interval = cli.block_interval,
        ranges = cli.ranges.max(1),
        prover = ?cli.prover,
        "sp1-worker starting"
    );

    // The async side is light (job-queue RPC and timers); proving runs on the blocking pool
    // one job at a time, and range proofs parallelize on their own scoped threads.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("sp1-worker")
        .worker_threads(2)
        .max_blocking_threads(4)
        .build()
        .context("failed to build tokio runtime")?;

    // The worker is a future that never resolves; block on it directly.
    runtime.block_on(worker);
    Ok(())
}

fn proof_config(
    network: Network,
    rollup_config_path: Option<&Path>,
    rollup_config_hash: Option<B256>,
) -> Result<(WorldRangeHardforkConfig, B256)> {
    if let Some(path) = rollup_config_path {
        let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        let value: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        let protocol_config = ProtocolHardforkConfig::from_rollup_config_value(&value)?;
        let hash = world_chain_proof_protocol::hash_rollup_config(&value)?;
        return Ok((range_hardfork_config(&protocol_config), hash));
    }

    let hash = rollup_config_hash
        .context("provide --rollup-config or ROLLUP_CONFIG, or supply --rollup-config-hash")?;
    let spec = network.chain_spec();
    let protocol_config = ProtocolHardforkConfig::from_chain_spec(spec.as_ref());
    Ok((range_hardfork_config(&protocol_config), hash))
}

fn range_hardfork_config(config: &ProtocolHardforkConfig) -> WorldRangeHardforkConfig {
    WorldRangeHardforkConfig {
        bedrock_block: config.bedrock_block,
        regolith_time: config.regolith_time,
        canyon_time: config.canyon_time,
        ecotone_time: config.ecotone_time,
        fjord_time: config.fjord_time,
        granite_time: config.granite_time,
        holocene_time: config.holocene_time,
        isthmus_time: config.isthmus_time,
        jovian_time: config.jovian_time,
        tropo_time: config.tropo_time,
        strato_time: config.strato_time,
    }
}
