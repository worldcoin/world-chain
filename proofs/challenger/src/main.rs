//! `world-chain-challenger` binary: scans WIP-1006 games in the OP dispute factory and
//! challenges any whose claimed output root disagrees with the canonical L2 root.
//!
//! Mirrors the in-process challenger wired by the devnet harness
//! (`crates/devnet/src/full_stack.rs::start_world_chain_challenger`), reading its
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
use world_chain_challenger::{
    AlloyChallengerClient, BondManager, BondManagerConfig, ChallengerClient, ChallengerConfig,
    OwnedGames, ResolutionManager, ResolutionManagerConfig, WorldChainChallenger,
};
use world_chain_proofs::OptimismConsensusClient;

#[derive(Debug, Parser)]
#[command(
    name = "world-chain-challenger",
    about = "World Chain proof-system challenger: challenges invalid output-root proposals on L1"
)]
struct Cli {
    /// Ethereum L1 execution RPC URL.
    #[arg(long, env = "L1_RPC_URL")]
    l1_rpc: String,

    /// op-node rollup RPC URL used to read canonical L2 output roots.
    #[arg(long, env = "OUTPUT_ROOT_RPC_URL")]
    output_root_rpc: String,

    /// Stock OP `DisputeGameFactory` address on L1.
    #[arg(long, env = "FACTORY_ADDRESS")]
    factory_address: Address,

    /// Hex-encoded private key the challenger signs L1 transactions with.
    #[arg(long, env = "CHALLENGER_KEY", hide_env_values = true)]
    challenger_key: PrivateKeySigner,

    /// Seconds between game-factory polls.
    #[arg(long, env = "POLL_INTERVAL_SECONDS", default_value_t = 12)]
    poll_interval_seconds: u64,

    /// Maximum number of games processed concurrently.
    #[arg(long, env = "MAX_GAME_CONCURRENCY", default_value_t = 10)]
    max_game_concurrency: usize,

    /// Maximum number of newly created games discovered per challenger tick.
    #[arg(long, env = "MAX_GAMES_PER_TICK", default_value_t = 100)]
    max_games_per_tick: u64,

    /// Seconds between challenger-owned game resolution passes.
    #[arg(
        long,
        env = "RESOLUTION_MANAGER_POLL_INTERVAL_SECONDS",
        default_value_t = 30
    )]
    resolution_manager_poll_interval_seconds: u64,

    /// Maximum number of game resolutions submitted per resolution pass.
    #[arg(long, env = "MAX_RESOLUTIONS_PER_TICK", default_value_t = 1)]
    max_resolutions_per_tick: usize,

    /// Seconds between challenger-bond discovery and withdrawal passes.
    #[arg(
        long,
        env = "BOND_MANAGER_POLL_INTERVAL_SECONDS",
        default_value_t = 300
    )]
    bond_manager_poll_interval_seconds: u64,

    /// Number of recent factory games scanned when the bond manager starts.
    #[arg(long, env = "BOND_MANAGER_INITIAL_SCAN_LIMIT", default_value_t = 1_000)]
    bond_manager_initial_scan_limit: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let challenger_address = cli.challenger_key.address();
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(cli.challenger_key))
        .connect_http(Url::parse(&cli.l1_rpc).context("invalid L1 RPC URL")?);

    let client = AlloyChallengerClient::new(provider, cli.factory_address);
    let output_roots = OptimismConsensusClient::new(cli.output_root_rpc.clone());
    let challenger_bond = client
        .challenger_bond()
        .await
        .context("failed to read challenger bond")?;
    let config = ChallengerConfig {
        challenger_bond,
        poll_interval: Duration::from_secs(cli.poll_interval_seconds),
        max_game_concurrency: cli.max_game_concurrency,
        max_games_per_tick: cli.max_games_per_tick,
    };
    let resolution_config = ResolutionManagerConfig {
        poll_interval: Duration::from_secs(cli.resolution_manager_poll_interval_seconds),
        max_resolutions_per_tick: cli.max_resolutions_per_tick,
    };
    let bond_manager_config = BondManagerConfig {
        poll_interval: Duration::from_secs(cli.bond_manager_poll_interval_seconds),
        initial_scan_limit: cli.bond_manager_initial_scan_limit,
    };
    let owned_games = OwnedGames::default();
    let mut challenger = WorldChainChallenger::with_owned_games(
        config,
        client.clone(),
        output_roots,
        owned_games.clone(),
    );
    let resolution_manager =
        ResolutionManager::new(resolution_config, client.clone(), owned_games.clone());
    let mut bond_manager = BondManager::new(bond_manager_config, client, owned_games);

    info!(
        l1_rpc_url = %cli.l1_rpc,
        output_root_rpc_url = %cli.output_root_rpc,
        factory = %cli.factory_address,
        challenger = %challenger_address,
        challenger_bond = ?challenger_bond,
        max_games_per_tick = cli.max_games_per_tick,
        resolution_manager_poll_interval_seconds =
            cli.resolution_manager_poll_interval_seconds,
        max_resolutions_per_tick = cli.max_resolutions_per_tick,
        bond_manager_poll_interval_seconds = cli.bond_manager_poll_interval_seconds,
        bond_manager_initial_scan_limit = cli.bond_manager_initial_scan_limit,
        "starting World Chain proof-system challenger"
    );

    tokio::select! {
        result = challenger.run_forever() => result.context("challenger stopped")?,
        result = resolution_manager.run_forever() => result.context("resolution manager stopped")?,
        result = bond_manager.run_forever() => result.context("bond manager stopped")?,
        _ = tokio::signal::ctrl_c() => info!("received ctrl-c, shutting down"),
    }
    Ok(())
}
