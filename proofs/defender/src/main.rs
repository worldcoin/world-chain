//! `world-chain-defender` binary: supplies initial proof support for valid WIP-1006 games,
//! escalates challenged games to the proof threshold, and resolves negative outcomes.
//!
//! Mirrors the in-process defender wired by the devnet harness
//! (`crates/devnet/src/full_stack.rs::start_world_chain_defender`), reading its
//! configuration from flags/environment so it can run as a standalone service.

use std::time::Duration;

use alloy_network::EthereumWallet;
use alloy_primitives::Address;
use alloy_provider::ProviderBuilder;
use alloy_signer_local::PrivateKeySigner;
use anyhow::{Context, Result};
use clap::Parser;
use tracing::info;
use url::Url;
use world_chain_defender::{
    AlloyDefenderClient, DEFAULT_GAME_SCAN_LOOKBACK, DEFAULT_L1_TX_CONFIRMATIONS, DefenderConfig,
    WorldChainDefender,
};
use world_chain_proofs::OptimismConsensusClient;
use world_chain_prover_service::RpcProverServiceClient;

#[derive(Debug, Parser)]
#[command(
    name = "world-chain-defender",
    about = "World Chain proof-system defender: proves, escalates, and resolves games"
)]
struct Cli {
    /// Ethereum L1 execution RPC URL.
    #[arg(long, env = "L1_RPC_URL")]
    l1_rpc: String,

    /// op-node rollup RPC URL used to read canonical L2 output roots.
    #[arg(long, env = "OUTPUT_ROOT_RPC_URL")]
    output_root_rpc: String,

    /// prover-service JSON-RPC URL.
    #[arg(long, env = "PROVER_SERVICE_URL")]
    prover_service_url: String,

    /// OP Stack `DisputeGameFactory` address on L1.
    #[arg(long, env = "FACTORY_ADDRESS")]
    factory_address: Address,

    /// Hex-encoded private key the defender signs L1 transactions with.
    #[arg(long, env = "DEFENDER_KEY", hide_env_values = true)]
    defender_key: PrivateKeySigner,

    /// The only proposer address whose games this defender will defend.
    #[arg(long, env = "ALLOWED_PROPOSER")]
    allowed_proposer: Address,

    /// Seconds between game-factory polls.
    #[arg(long, env = "POLL_INTERVAL_SECONDS", default_value_t = 12)]
    poll_interval_seconds: u64,

    /// Maximum number of games processed concurrently.
    #[arg(long, env = "MAX_GAME_CONCURRENCY", default_value_t = 10)]
    max_game_concurrency: usize,

    /// Maximum number of newly created games discovered per defender tick.
    #[arg(long, env = "MAX_GAMES_PER_TICK", default_value_t = 100)]
    max_games_per_tick: u64,

    /// Number of previously scanned games reconsidered per defender tick.
    #[arg(
        long,
        env = "GAME_SCAN_LOOKBACK",
        default_value_t = DEFAULT_GAME_SCAN_LOOKBACK
    )]
    game_scan_lookback: u64,

    /// Number of L1 confirmations required before a proof submission is accepted.
    #[arg(
        long,
        env = "L1_TX_CONFIRMATIONS",
        default_value_t = DEFAULT_L1_TX_CONFIRMATIONS,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    l1_tx_confirmations: u64,

    /// Maximum number of negatively resolvable games settled per defender tick.
    #[arg(long, env = "MAX_RESOLUTIONS_PER_TICK", default_value_t = 10)]
    max_resolutions_per_tick: usize,

    /// Conservative upper bound on the age of a game with an open proof window.
    #[arg(long, env = "MAX_GAME_AGE_SECONDS", default_value_t = 604_800)]
    max_game_age_seconds: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let defender_address = cli.defender_key.address();
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(cli.defender_key))
        .connect_http(Url::parse(&cli.l1_rpc).context("invalid L1 RPC URL")?);

    let client = AlloyDefenderClient::new(provider, cli.factory_address, cli.l1_tx_confirmations);
    let output_roots = OptimismConsensusClient::new(cli.output_root_rpc.clone());
    let proof_requester = RpcProverServiceClient::new(&cli.prover_service_url)
        .with_context(|| format!("failed to connect to {}", cli.prover_service_url))?;
    let config = DefenderConfig {
        allowed_proposer: cli.allowed_proposer,
        poll_interval: Duration::from_secs(cli.poll_interval_seconds),
        max_game_concurrency: cli.max_game_concurrency,
        max_games_per_tick: cli.max_games_per_tick,
        game_scan_lookback: cli.game_scan_lookback,
        max_resolutions_per_tick: cli.max_resolutions_per_tick,
        max_game_age: Duration::from_secs(cli.max_game_age_seconds),
    };
    let mut defender = WorldChainDefender::new(config, client, output_roots, proof_requester);

    info!(
        l1_rpc_url = %cli.l1_rpc,
        output_root_rpc_url = %cli.output_root_rpc,
        prover_service = %cli.prover_service_url,
        dispute_game_factory = %cli.factory_address,
        defender = %defender_address,
        allowed_proposer = %cli.allowed_proposer,
        max_games_per_tick = cli.max_games_per_tick,
        game_scan_lookback = cli.game_scan_lookback,
        l1_tx_confirmations = cli.l1_tx_confirmations,
        max_resolutions_per_tick = cli.max_resolutions_per_tick,
        "starting World Chain proof-system defender"
    );

    tokio::select! {
        result = defender.run_forever() => result.context("defender stopped")?,
        _ = tokio::signal::ctrl_c() => info!("received ctrl-c, shutting down"),
    }
    Ok(())
}
