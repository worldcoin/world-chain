//! `world-chain-proposer` binary: watches L2 output roots and opens
//! `WorldChainProofSystemGame` proposals on L1 through the proof-system factory.
//!
//! Mirrors the in-process proposer wired by the devnet harness
//! (`crates/devnet/src/full_stack.rs::start_world_chain_proposer`), reading its
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
use world_chain_proofs::OptimismConsensusClient;
use world_chain_proposer::{
    AlloyProofSystemClient, BondManager, BondManagerConfig, ProposerClient, ProposerConfig,
    WorldChainProposer,
};

#[derive(Debug, Parser)]
#[command(
    name = "world-chain-proposer",
    about = "World Chain proof-system proposer: opens output-root proposals on L1"
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

    /// Stock OP `AnchorStateRegistry` address on L1.
    #[arg(long, env = "ANCHOR_REGISTRY_ADDRESS")]
    anchor_registry_address: Address,

    /// Hex-encoded private key the proposer signs L1 transactions with.
    #[arg(long, env = "PROPOSER_KEY", hide_env_values = true)]
    proposer_key: PrivateKeySigner,

    /// L2 blocks between a proposal's parent and its claimed block.
    #[arg(long, env = "BLOCK_INTERVAL")]
    block_interval: u64,

    /// Seconds between output-root polls.
    #[arg(long, env = "POLL_INTERVAL_SECONDS", default_value_t = 12)]
    poll_interval_seconds: u64,

    /// Maximum game-resolution transactions submitted during one proposer tick.
    #[arg(long, env = "MAX_RESOLUTIONS_PER_TICK", default_value_t = 1)]
    max_resolutions_per_tick: usize,

    /// Seconds between proposer-bond discovery and withdrawal passes.
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

    let proposer_address = cli.proposer_key.address();
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(cli.proposer_key))
        .connect_http(Url::parse(&cli.l1_rpc).context("invalid L1 RPC URL")?);

    let contracts =
        AlloyProofSystemClient::new(provider, cli.factory_address, cli.anchor_registry_address);
    let bond_manager_config = BondManagerConfig {
        poll_interval: Duration::from_secs(cli.bond_manager_poll_interval_seconds),
        initial_scan_limit: cli.bond_manager_initial_scan_limit,
    };
    let mut bond_manager = BondManager::new(bond_manager_config, contracts.clone());
    let output_roots = OptimismConsensusClient::new(cli.output_root_rpc.clone());
    let proposer_bond = contracts
        .proposer_bond()
        .await
        .context("failed to read proposer bond")?;
    let config = ProposerConfig {
        block_interval: cli.block_interval,
        proposer_bond,
        poll_interval: Duration::from_secs(cli.poll_interval_seconds),
        max_resolutions_per_tick: cli.max_resolutions_per_tick,
    };
    let proposer = WorldChainProposer::new(config, contracts, output_roots);

    info!(
        l1_rpc_url = %cli.l1_rpc,
        output_root_rpc_url = %cli.output_root_rpc,
        factory = %cli.factory_address,
        anchor = %cli.anchor_registry_address,
        proposer = %proposer_address,
        block_interval = cli.block_interval,
        max_resolutions_per_tick = cli.max_resolutions_per_tick,
        bond_manager_poll_interval_seconds = cli.bond_manager_poll_interval_seconds,
        bond_manager_initial_scan_limit = cli.bond_manager_initial_scan_limit,
        "starting World Chain proof-system proposer"
    );

    tokio::select! {
        result = proposer.run_forever() => result.context("proposer stopped")?,
        result = bond_manager.run_forever() => result.context("bond manager stopped")?,
        _ = tokio::signal::ctrl_c() => info!("received ctrl-c, shutting down"),
    }
    Ok(())
}
