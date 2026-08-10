//! `world-chain-challenger` binary: scans WIP-1006 games on the OP `DisputeGameFactory` and
//! challenges any whose claimed output root disagrees with the canonical L2 root.
//!
//! Mirrors the in-process challenger wired by the devnet harness
//! (`crates/devnet/src/full_stack.rs::start_world_chain_challenger`), reading its
//! configuration from flags/environment so it can run as a standalone service.

use std::time::Duration;

use alloy_primitives::Address;
use alloy_provider::ProviderBuilder;
use alloy_signer_local::PrivateKeySigner;
use anyhow::{Context, Result};
use clap::{ArgGroup, Parser};
use tracing::info;
use url::Url;
use world_chain_challenger::{
    AlloyChallengerClient, BondManager, BondManagerConfig, ChallengerClient, ChallengerConfig,
    DEFAULT_GAME_SCAN_LOOKBACK, DEFAULT_L1_TX_CONFIRMATIONS, OwnedGames, ResolutionManager,
    ResolutionManagerConfig, WorldChainChallenger,
};
use world_chain_proof_protocol::{OptimismConsensusClient, VerifyingConsensusProvider};
use world_chain_proof_tx_signer::build_transaction_signer;

#[derive(Debug, Parser)]
#[command(
    name = "world-chain-challenger",
    about = "World Chain proof-system challenger: challenges invalid output-root proposals on L1",
    group = ArgGroup::new("transaction_signer")
        .required(true)
        .multiple(false)
        .args(["challenger_key", "challenger_kms_key_id"])
)]
struct Cli {
    /// Ethereum L1 execution RPC URL.
    #[arg(long, env = "L1_RPC_URL")]
    l1_rpc: String,

    /// op-node rollup RPC URL used to read canonical L2 output roots.
    #[arg(long, env = "OUTPUT_ROOT_RPC_URL")]
    output_root_rpc: String,

    /// Optional verifying op-node rollup RPC URL. Every result must match the primary endpoint.
    #[arg(long, env = "VERIFYING_OUTPUT_ROOT_RPC_URL")]
    verifying_output_root_rpc: Option<String>,

    /// OP Stack `DisputeGameFactory` address on L1.
    #[arg(long, env = "FACTORY_ADDRESS")]
    factory_address: Address,

    /// Hex-encoded private key the challenger signs L1 transactions with.
    #[arg(long, env = "CHALLENGER_KEY", hide_env_values = true)]
    challenger_key: Option<PrivateKeySigner>,

    /// AWS KMS key ID or alias the challenger signs L1 transactions with.
    #[arg(long, env = "CHALLENGER_KMS_KEY_ID", hide_env_values = true)]
    challenger_kms_key_id: Option<String>,

    /// Seconds between game-factory polls.
    #[arg(long, env = "POLL_INTERVAL_SECONDS", default_value_t = 12)]
    poll_interval_seconds: u64,

    /// Maximum number of games processed concurrently.
    #[arg(long, env = "MAX_GAME_CONCURRENCY", default_value_t = 10)]
    max_game_concurrency: usize,

    /// Maximum number of newly created games discovered per challenger tick.
    #[arg(long, env = "MAX_GAMES_PER_TICK", default_value_t = 100)]
    max_games_per_tick: u64,

    /// Number of previously scanned games reconsidered per challenger tick.
    #[arg(
        long,
        env = "GAME_SCAN_LOOKBACK",
        default_value_t = DEFAULT_GAME_SCAN_LOOKBACK
    )]
    game_scan_lookback: u64,

    /// Number of L1 confirmations required before a transaction is accepted.
    #[arg(
        long,
        env = "L1_TX_CONFIRMATIONS",
        default_value_t = DEFAULT_L1_TX_CONFIRMATIONS,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    l1_tx_confirmations: u64,

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

    /// Per-request timeout applied to every L1 RPC call, in seconds.
    #[arg(
        long,
        env = "L1_RPC_TIMEOUT_SECONDS",
        default_value_t = world_chain_proof_metrics::DEFAULT_RPC_REQUEST_TIMEOUT_SECONDS,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    l1_rpc_timeout_seconds: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let _telemetry_guard = telemetry_batteries::init()
        .map_err(|error| anyhow::anyhow!("failed to initialize telemetry: {error:#}"))?;
    world_chain_proof_metrics::describe_metrics();

    let cli = Cli::parse();

    let l1_rpc_url = Url::parse(&cli.l1_rpc).context("invalid L1 RPC URL")?;
    let wallet =
        build_transaction_signer(cli.challenger_key, cli.challenger_kms_key_id, &l1_rpc_url)
            .await
            .context("failed to initialize challenger signer")?
            .wallet();
    let challenger_address = wallet.default_signer().address();
    let l1_rpc_client = world_chain_proof_metrics::metered_http_client(
        l1_rpc_url,
        world_chain_proof_metrics::RPC_TARGET_L1_EXECUTION,
        Duration::from_secs(cli.l1_rpc_timeout_seconds),
    )
    .context("failed to build the L1 RPC client")?;
    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect_client(l1_rpc_client);
    world_chain_proof_metrics::refresh_wallet_balance(&provider, challenger_address).await;

    let client = AlloyChallengerClient::new(provider, cli.factory_address, cli.l1_tx_confirmations);

    // Preflight the factory index before entering the scan loop. Crash instead of reporting the
    // process alive while every scan tick fails.
    let game_count = ChallengerClient::game_count(&client)
        .await
        .with_context(|| {
            format!(
                "failed to read gameCount() from the dispute game factory at {} over {} — \
                 check the factory address and the L1 RPC endpoint",
                cli.factory_address,
                world_chain_proof_metrics::redact_endpoint(&cli.l1_rpc),
            )
        })?;

    let output_roots = VerifyingConsensusProvider::new(
        OptimismConsensusClient::new(cli.output_root_rpc.clone()),
        cli.verifying_output_root_rpc
            .clone()
            .map(OptimismConsensusClient::new),
    );
    let config = ChallengerConfig {
        poll_interval: Duration::from_secs(cli.poll_interval_seconds),
        max_game_concurrency: cli.max_game_concurrency,
        max_games_per_tick: cli.max_games_per_tick,
        game_scan_lookback: cli.game_scan_lookback,
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
        l1_rpc_url = world_chain_proof_metrics::redact_endpoint(&cli.l1_rpc),
        output_root_rpc_url = world_chain_proof_metrics::redact_endpoint(&cli.output_root_rpc),
        verifying_output_root_rpc_configured = cli.verifying_output_root_rpc.is_some(),
        dispute_game_factory = %cli.factory_address,
        factory_game_count = game_count,
        challenger = %challenger_address,
        max_games_per_tick = cli.max_games_per_tick,
        game_scan_lookback = cli.game_scan_lookback,
        l1_tx_confirmations = cli.l1_tx_confirmations,
        resolution_manager_poll_interval_seconds =
            cli.resolution_manager_poll_interval_seconds,
        max_resolutions_per_tick = cli.max_resolutions_per_tick,
        bond_manager_poll_interval_seconds = cli.bond_manager_poll_interval_seconds,
        bond_manager_initial_scan_limit = cli.bond_manager_initial_scan_limit,
        l1_rpc_timeout_seconds = cli.l1_rpc_timeout_seconds,
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
