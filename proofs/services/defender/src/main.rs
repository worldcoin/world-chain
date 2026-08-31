//! `world-chain-defender` binary: supplies proof support for the valid WIP-1006 games selected
//! from the current anchor and escalates challenged games to the proof threshold.
//!
//! Mirrors the in-process defender wired by the devnet harness
//! (`crates/devnet/src/full_stack.rs::start_world_chain_defender`), reading its
//! configuration from flags/environment so it can run as a standalone service.

use std::time::Duration;

use alloy_primitives::Address;
use alloy_provider::ProviderBuilder;
use alloy_signer_local::PrivateKeySigner;
use anyhow::{Context, Result};
use clap::{ArgGroup, Parser};
use tracing::{info, warn};
use url::Url;
use world_chain_defender::{
    AlloyDefenderClient, DEFAULT_L1_TX_CONFIRMATIONS, DefenderConfig, WorldChainDefender,
};
use world_chain_proof_protocol::{
    IDisputeGameFactory, IERC20StakingVault, OptimismConsensusClient, VerifyingConsensusProvider,
    read_registered_bond_vault,
};
use world_chain_proof_tx_signer::build_transaction_signer;
use world_chain_prover_service::RpcProverServiceClient;

#[derive(Debug, Parser)]
#[command(
    name = "world-chain-defender",
    about = "World Chain proof-system defender: proves the lineage selected from the anchor",
    group = ArgGroup::new("transaction_signer")
        .required(true)
        .multiple(false)
        .args(["defender_key", "defender_kms_key_id"])
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

    /// prover-service JSON-RPC URL.
    #[arg(long, env = "PROVER_SERVICE_URL")]
    prover_service_url: String,

    /// OP Stack `DisputeGameFactory` address on L1.
    #[arg(long, env = "FACTORY_ADDRESS")]
    factory_address: Address,

    /// Hex-encoded private key the defender signs L1 transactions with.
    #[arg(long, env = "DEFENDER_KEY", hide_env_values = true)]
    defender_key: Option<PrivateKeySigner>,

    /// AWS KMS key ID or alias the defender signs L1 transactions with.
    #[arg(long, env = "DEFENDER_KMS_KEY_ID", hide_env_values = true)]
    defender_kms_key_id: Option<String>,

    /// Address credited each submitted lane's share of a forfeited challenger bond.
    /// Defaults to the defender signer.
    #[arg(long, env = "PROOF_REWARD_RECIPIENT")]
    proof_reward_recipient: Option<Address>,

    /// Seconds between selected-lineage scans.
    #[arg(long, env = "POLL_INTERVAL_SECONDS", default_value_t = 12)]
    poll_interval_seconds: u64,

    /// Maximum number of games processed concurrently.
    #[arg(long, env = "MAX_GAME_CONCURRENCY", default_value_t = 10)]
    max_game_concurrency: usize,

    /// Number of L1 confirmations required before a proof submission is accepted.
    #[arg(
        long,
        env = "L1_TX_CONFIRMATIONS",
        default_value_t = DEFAULT_L1_TX_CONFIRMATIONS,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    l1_tx_confirmations: u64,

    /// Maximum seconds to wait for an L1 transaction receipt and required confirmations.
    #[arg(
        long,
        env = "L1_TX_RECEIPT_TIMEOUT_SECONDS",
        default_value_t = world_chain_proof_protocol::DEFAULT_L1_TX_RECEIPT_TIMEOUT_SECONDS,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    l1_tx_receipt_timeout_seconds: u64,

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
    let wallet = build_transaction_signer(cli.defender_key, cli.defender_kms_key_id, &l1_rpc_url)
        .await
        .context("failed to initialize defender signer")?
        .wallet();
    let defender_address = wallet.default_signer().address();
    let reward_recipient = cli.proof_reward_recipient.unwrap_or(defender_address);
    let l1_rpc_client = world_chain_proof_metrics::metered_http_client(
        l1_rpc_url,
        world_chain_proof_metrics::RPC_TARGET_L1_EXECUTION,
        Duration::from_secs(cli.l1_rpc_timeout_seconds),
    )
    .context("failed to build the L1 RPC client")?;
    let provider = ProviderBuilder::new()
        .disable_recommended_fillers()
        .with_gas_estimation()
        .with_blob_gas_estimation()
        .with_simple_nonce_management()
        .fetch_chain_id()
        .wallet(wallet)
        .connect_client(l1_rpc_client);
    world_chain_proof_metrics::refresh_wallet_balance(&provider, defender_address).await;
    let factory = IDisputeGameFactory::IDisputeGameFactoryInstance::new(
        cli.factory_address,
        provider.clone(),
    );
    match read_registered_bond_vault(&provider, &factory).await {
        Ok(bond_vault) => {
            let vault =
                IERC20StakingVault::IERC20StakingVaultInstance::new(bond_vault, provider.clone());
            match vault.availableBalance(reward_recipient).call().await {
                Ok(balance) => world_chain_proof_metrics::record_vault_balance(
                    bond_vault,
                    reward_recipient,
                    "defender",
                    balance,
                ),
                Err(error) => {
                    warn!(%error, "failed to fetch defender reward-recipient ERC-20 vault balance")
                }
            }
        }
        Err(error) => warn!(%error, "failed to discover ERC-20 vault for defender telemetry"),
    }

    let client = AlloyDefenderClient::new(
        provider,
        cli.factory_address,
        cli.l1_tx_confirmations,
        Duration::from_secs(cli.l1_tx_receipt_timeout_seconds),
        reward_recipient,
    )
    .await
    .context("failed to connect defender to the registered proof system")?;
    let output_roots = VerifyingConsensusProvider::new(
        OptimismConsensusClient::new(cli.output_root_rpc.clone()),
        cli.verifying_output_root_rpc
            .clone()
            .map(OptimismConsensusClient::new),
    );
    let proof_requester = RpcProverServiceClient::new(&cli.prover_service_url)
        .with_context(|| format!("failed to connect to {}", cli.prover_service_url))?;
    let config = DefenderConfig {
        poll_interval: Duration::from_secs(cli.poll_interval_seconds),
        max_game_concurrency: cli.max_game_concurrency,
    };
    let mut defender = WorldChainDefender::new(config, client, output_roots, proof_requester);

    info!(
        l1_rpc_url = world_chain_proof_metrics::redact_endpoint(&cli.l1_rpc),
        output_root_rpc_url = world_chain_proof_metrics::redact_endpoint(&cli.output_root_rpc),
        verifying_output_root_rpc_configured = cli.verifying_output_root_rpc.is_some(),
        prover_service = %cli.prover_service_url,
        dispute_game_factory = %cli.factory_address,
        defender = %defender_address,
        reward_recipient = %reward_recipient,
        l1_tx_confirmations = cli.l1_tx_confirmations,
        l1_tx_receipt_timeout_seconds = cli.l1_tx_receipt_timeout_seconds,
        l1_rpc_timeout_seconds = cli.l1_rpc_timeout_seconds,
        "starting World Chain proof-system defender"
    );

    tokio::select! {
        result = defender.run_forever() => result.context("defender stopped")?,
        _ = tokio::signal::ctrl_c() => info!("received ctrl-c, shutting down"),
    }
    Ok(())
}
