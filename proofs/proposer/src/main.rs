//! `world-chain-proposer` binary: watches L2 output roots and opens WIP-1006
//! `MultiProofGame` proposals on L1 through the stock OP `DisputeGameFactory`.
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
use world_chain_proofs::{OptimismConsensusClient, VerifyingConsensusProvider};
use world_chain_proposer::{
    AlloyProofSystemClient, BondManager, BondManagerConfig, ProposerConfig, WorldChainProposer,
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

    /// Optional verifying op-node rollup RPC URL. Every result must match the primary endpoint.
    #[arg(long, env = "VERIFYING_OUTPUT_ROOT_RPC_URL")]
    verifying_output_root_rpc: Option<String>,

    /// OP Stack `DisputeGameFactory` address on L1.
    #[arg(long, env = "FACTORY_ADDRESS")]
    factory_address: Address,

    /// Hex-encoded private key the proposer signs L1 transactions with.
    #[arg(long, env = "PROPOSER_KEY", hide_env_values = true)]
    proposer_key: PrivateKeySigner,

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

    /// Number of confirmations to require after sending a tx onchain.
    #[arg(long, env = "CONFIRMATIONS", default_value_t = 5)]
    confirmations: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let _telemetry_guard = telemetry_batteries::init()
        .map_err(|error| anyhow::anyhow!("failed to initialize telemetry: {error:#}"))?;
    world_chain_proposer::metrics::describe_metrics();

    let cli = Cli::parse();

    let proposer_address = cli.proposer_key.address();
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(cli.proposer_key))
        .connect_http(Url::parse(&cli.l1_rpc).context("invalid L1 RPC URL")?);

    let contracts = AlloyProofSystemClient::new(provider, cli.factory_address, cli.confirmations)
        .await
        .context("failed to bind the World Chain proof system")?;
    let bond_manager_config = BondManagerConfig {
        poll_interval: Duration::from_secs(cli.bond_manager_poll_interval_seconds),
        initial_scan_limit: cli.bond_manager_initial_scan_limit,
    };
    let mut bond_manager = BondManager::new(bond_manager_config, contracts.clone());
    let output_roots = VerifyingConsensusProvider::new(
        OptimismConsensusClient::new(cli.output_root_rpc.clone()),
        cli.verifying_output_root_rpc
            .clone()
            .map(OptimismConsensusClient::new),
    );
    let registered = contracts.registered_lineage_config();
    let config = ProposerConfig {
        poll_interval: Duration::from_secs(cli.poll_interval_seconds),
        max_resolutions_per_tick: cli.max_resolutions_per_tick,
    };
    let proposer = WorldChainProposer::new(config, contracts, output_roots);

    info!(
        l1_rpc_url = %cli.l1_rpc,
        output_root_rpc_url = %cli.output_root_rpc,
        verifying_output_root_rpc_configured = cli.verifying_output_root_rpc.is_some(),
        dispute_game_factory = %cli.factory_address,
        anchor = %registered.anchor_registry,
        proposer = %proposer_address,
        domain_hash = %registered.domain_hash,
        block_interval = registered.block_interval,
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
