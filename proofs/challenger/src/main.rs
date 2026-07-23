//! `world-chain-challenger` binary: scans `WorldChainProofSystemFactory` games and
//! challenges any whose claimed output root disagrees with the canonical L2 root.
//!
//! Mirrors the in-process challenger wired by the devnet harness
//! (`crates/devnet/src/full_stack.rs::start_world_chain_challenger`), reading its
//! configuration from flags/environment so it can run as a standalone service.

use std::time::Duration;

use alloy_network::EthereumWallet;
use alloy_primitives::{Address, U256};
use alloy_provider::ProviderBuilder;
use alloy_signer_local::PrivateKeySigner;
use anyhow::{Context, Result};
use clap::Parser;
use tracing::info;
use url::Url;
use world_chain_challenger::{AlloyChallengerClient, ChallengerConfig, WorldChainChallenger};
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

    /// `WorldChainProofSystemFactory` address on L1.
    #[arg(long, env = "FACTORY_ADDRESS")]
    factory_address: Address,

    /// World Chain game type registered on the DisputeGameFactory.
    #[arg(long, env = "GAME_TYPE", default_value_t = 42)]
    game_type: u32,

    /// Hex-encoded private key the challenger signs L1 transactions with.
    #[arg(long, env = "CHALLENGER_KEY", hide_env_values = true)]
    challenger_key: PrivateKeySigner,

    /// Bond posted with each challenge, in wei (default 0.1 ETH).
    #[arg(
        long,
        env = "CHALLENGER_BOND_WEI",
        default_value_t = 100_000_000_000_000_000
    )]
    challenger_bond_wei: u128,

    /// Seconds between game-factory polls.
    #[arg(long, env = "POLL_INTERVAL_SECONDS", default_value_t = 12)]
    poll_interval_seconds: u64,

    /// Maximum number of games processed concurrently.
    #[arg(long, env = "MAX_GAME_CONCURRENCY", default_value_t = 10)]
    max_game_concurrency: usize,
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

    let client = AlloyChallengerClient::new(provider, cli.factory_address, cli.game_type);
    let output_roots = OptimismConsensusClient::new(cli.output_root_rpc.clone());
    let config = ChallengerConfig {
        challenger_bond: U256::from(cli.challenger_bond_wei),
        poll_interval: Duration::from_secs(cli.poll_interval_seconds),
        max_game_concurrency: cli.max_game_concurrency,
    };
    let mut challenger = WorldChainChallenger::new(config, client, output_roots);

    info!(
        l1_rpc_url = %cli.l1_rpc,
        output_root_rpc_url = %cli.output_root_rpc,
        factory = %cli.factory_address,
        challenger = %challenger_address,
        "starting World Chain proof-system challenger"
    );

    tokio::select! {
        result = challenger.run_forever() => result.context("challenger stopped")?,
        _ = tokio::signal::ctrl_c() => info!("received ctrl-c, shutting down"),
    }
    Ok(())
}
