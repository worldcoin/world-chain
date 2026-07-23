//! `world-chain-defender` binary: watches challenged valid `WorldChainProofSystemFactory`
//! games, requests proofs from the prover-service, and submits completed proof lanes on L1.
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
use world_chain_defender::{AlloyDefenderClient, DefenderConfig, WorldChainDefender};
use world_chain_proofs::OptimismConsensusClient;
use world_chain_prover_service::RpcProverServiceClient;

#[derive(Debug, Parser)]
#[command(
    name = "world-chain-defender",
    about = "World Chain proof-system defender: proves and submits lanes for challenged games"
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

    /// `WorldChainProofSystemFactory` address on L1.
    #[arg(long, env = "FACTORY_ADDRESS")]
    factory_address: Address,

    /// World Chain game type registered on the DisputeGameFactory.
    #[arg(long, env = "GAME_TYPE", default_value_t = 42)]
    game_type: u32,

    /// Hex-encoded private key the defender signs L1 transactions with.
    #[arg(long, env = "DEFENDER_KEY", hide_env_values = true)]
    defender_key: PrivateKeySigner,

    /// Seconds between game-factory polls.
    #[arg(long, env = "POLL_INTERVAL_SECONDS", default_value_t = 12)]
    poll_interval_seconds: u64,

    /// Maximum number of games processed concurrently.
    #[arg(long, env = "MAX_GAME_CONCURRENCY", default_value_t = 10)]
    max_game_concurrency: usize,

    /// Maximum proof attempts per challenged game before giving up.
    #[arg(long, env = "MAX_PROOF_ATTEMPTS", default_value_t = 3)]
    max_proof_attempts: u32,
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

    let client = AlloyDefenderClient::new(provider, cli.factory_address, cli.game_type);
    let output_roots = OptimismConsensusClient::new(cli.output_root_rpc.clone());
    let proof_requester = RpcProverServiceClient::new(&cli.prover_service_url)
        .with_context(|| format!("failed to connect to {}", cli.prover_service_url))?;
    let config = DefenderConfig {
        poll_interval: Duration::from_secs(cli.poll_interval_seconds),
        max_game_concurrency: cli.max_game_concurrency,
        max_proof_attempts: cli.max_proof_attempts,
    };
    let mut defender = WorldChainDefender::new(config, client, output_roots, proof_requester);

    info!(
        l1_rpc_url = %cli.l1_rpc,
        output_root_rpc_url = %cli.output_root_rpc,
        prover_service = %cli.prover_service_url,
        factory = %cli.factory_address,
        defender = %defender_address,
        "starting World Chain proof-system defender"
    );

    tokio::select! {
        result = defender.run_forever() => result.context("defender stopped")?,
        _ = tokio::signal::ctrl_c() => info!("received ctrl-c, shutting down"),
    }
    Ok(())
}
