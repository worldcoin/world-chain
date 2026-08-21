use std::{path::PathBuf, sync::Arc, time::Duration};

use alloy_primitives::{Address, B256, U256};
use alloy_provider::ProviderBuilder;
use anyhow::{Context, Result};
use clap::{ArgGroup, Parser};
use world_chain_chainspec::WorldChainSpec;
use world_chain_proof_kona_host::online::{
    OnlineHostConfig, build_online_config, hardfork_config_from_chain_spec,
};
use world_chain_proof_protocol::AlloyProofGameProvider;
use world_chain_proof_sp1_host::{
    Sp1ProverKind, WorldSuccinctProver,
    cpu_prover::{CpuSuccinctProver, SP1ProofMode},
    mock_prover::MockSuccinctProver,
    network_prover::{
        NetworkConnection, NetworkCreditClient, NetworkProofRequestConfig, NetworkProverLimits,
        NetworkSuccinctProver, ProofLimits, SignerType,
    },
    vkeys::embedded_vkey_manifest,
};
use world_chain_proof_sp1_worker::{
    ProofWorker, ProofWorkerConfig, RetryConfig, Sp1Backend, Sp1BackendConfig,
};
use world_chain_proof_worker::WorkerHeartbeatConfig;
use world_chain_prover_service::RpcProverServiceClient;

use super::{
    deposit::{DepositReceipt, parse_prove_amount, submit_deposit},
    select_network_signer,
    succinct::{SettlementConfig, format_prove, load_settlement_config, prove_as_f64},
};

const DEFAULT_SUBMIT_PROOF_RETRY_MAX_RETRIES: usize = 10;
const DEFAULT_SUBMIT_PROOF_RETRY_INITIAL_DELAY_MS: u64 = 100;
const DEFAULT_SUBMIT_PROOF_RETRY_MAX_DELAY_MS: u64 = 10_000;
const DEFAULT_WORKER_HEARTBEAT_INTERVAL_SEC: u64 = 30;
const DEFAULT_WORKER_MAX_CONSECUTIVE_HEARTBEAT_FAILURES: u32 = 5;
const SP1_NETWORK_BALANCE_POLL_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_SP1_NETWORK_MINIMUM_BALANCE: &str = "10";
const DEFAULT_SP1_RANGE_CYCLE_LIMIT: u64 = 1_500_000_000_000;
const DEFAULT_SP1_RANGE_GAS_LIMIT: u64 = 1_300_000_000_000;
const DEFAULT_SP1_AGGREGATION_CYCLE_LIMIT: u64 = 7_000_000;
const DEFAULT_SP1_AGGREGATION_GAS_LIMIT: u64 = 6_500_000;

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
    group = ArgGroup::new("sp1_signer")
        .required(true)
        .multiple(false)
        .args(["sp1_private_key", "sp1_kms_key_id"])
)]
pub struct WorkerArgs {
    /// prover-service JSON-RPC URL.
    #[arg(long, env = "PROVER_SERVICE_URL")]
    prover_service_url: String,

    /// World Chain L2 execution RPC URL.
    #[arg(long, env = "L2_RPC_URL")]
    l2_rpc: String,

    /// L2 consensus RPC serving `optimism_outputAtBlock`, used only as the `eth_getProof`
    /// fallback. Must not be the execution RPC.
    #[arg(long, env = "L2_CONSENSUS_RPC_URL")]
    l2_consensus_rpc: Option<String>,

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

    /// Allow proving blocks newer than the finalized L2 head.
    #[arg(long)]
    allow_unfinalized: bool,

    /// Maximum seconds to spend generating one Kona witness.
    #[arg(long, default_value_t = 900)]
    witness_timeout_seconds: u64,

    /// Prover backend.
    #[arg(
        long,
        env = "SP1_PROVER",
        default_value_t = Sp1ProverKind::Cpu
    )]
    prover: Sp1ProverKind,

    /// SP1 network private key.
    ///
    /// Exactly one of this and sp1_kms_key_id must be configured.
    #[arg(
        long,
        env = "SP1_PRIVATE_KEY",
        value_parser = clap::builder::NonEmptyStringValueParser::new()
    )]
    sp1_private_key: Option<String>,

    /// AWS KMS key ID or alias the sp1 proof worker signs proof requests.
    ///
    /// Exactly one of this and sp1_private_key must be configured.
    #[arg(
        long,
        env = "SP1_KMS_KEY_ID",
        hide_env_values = true,
        value_parser = clap::builder::NonEmptyStringValueParser::new()
    )]
    sp1_kms_key_id: Option<String>,

    /// Ethereum mainnet RPC used to validate the Succinct VApp and its deposit threshold.
    /// Required when --prover network.
    #[arg(long, env = "SP1_NETWORK_L1_RPC_URL")]
    sp1_network_l1_rpc_url: Option<String>,

    /// SuccinctVApp proxy address on Ethereum mainnet. Required when --prover network.
    #[arg(long, env = "SUCCINCT_VAPP_ADDRESS")]
    succinct_vapp_address: Option<Address>,

    /// Minimum SP1 Network credit balance in human-readable PROVE.
    #[arg(
        long,
        env = "SP1_NETWORK_MINIMUM_BALANCE",
        default_value = DEFAULT_SP1_NETWORK_MINIMUM_BALANCE
    )]
    sp1_network_minimum_balance: String,

    /// Minimum amount of PROVE to deposit when refilling SP1 Network credits.
    /// Defaults to SuccinctVApp.minDepositAmount().
    #[arg(long, env = "SP1_NETWORK_REFILL_AMOUNT")]
    sp1_network_refill_amount: Option<String>,

    /// Seconds to sleep between job-queue polls when no work is available.
    #[arg(long, default_value_t = 10)]
    poll_interval_seconds: u64,

    /// Execute each SP1 guest locally to estimate its cycle and gas limits before submitting it.
    /// By default, the worker skips local execution and uses the configured fixed limits.
    #[arg(long, env = "SP1_ESTIMATE_LIMITS")]
    sp1_estimate_limits: bool,

    /// Cycle limit for optimistic SP1 range-proof requests.
    #[arg(
        long,
        env = "SP1_RANGE_CYCLE_LIMIT",
        default_value_t = DEFAULT_SP1_RANGE_CYCLE_LIMIT,
        conflicts_with = "sp1_estimate_limits",
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    sp1_range_cycle_limit: u64,

    /// Gas limit in PGUs for optimistic SP1 range-proof requests.
    #[arg(
        long,
        env = "SP1_RANGE_GAS_LIMIT",
        default_value_t = DEFAULT_SP1_RANGE_GAS_LIMIT,
        conflicts_with = "sp1_estimate_limits",
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    sp1_range_gas_limit: u64,

    /// Cycle limit for optimistic SP1 aggregation-proof requests.
    #[arg(
        long,
        env = "SP1_AGGREGATION_CYCLE_LIMIT",
        default_value_t = DEFAULT_SP1_AGGREGATION_CYCLE_LIMIT,
        conflicts_with = "sp1_estimate_limits",
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    sp1_aggregation_cycle_limit: u64,

    /// Gas limit in PGUs for optimistic SP1 aggregation-proof requests.
    #[arg(
        long,
        env = "SP1_AGGREGATION_GAS_LIMIT",
        default_value_t = DEFAULT_SP1_AGGREGATION_GAS_LIMIT,
        conflicts_with = "sp1_estimate_limits",
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    sp1_aggregation_gas_limit: u64,

    /// Maximum auction price in PROVE base units per PGU. Uses the SP1 Network default if omitted.
    #[arg(
        long,
        env = "SP1_MAX_PRICE_PER_PGU",
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    sp1_max_price_per_pgu: Option<u64>,

    /// Maximum seconds a network request may remain unassigned. Uses the SP1 SDK default if
    /// omitted.
    #[arg(
        long,
        env = "SP1_AUCTION_TIMEOUT_SECONDS",
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    sp1_auction_timeout_seconds: Option<u64>,

    /// Overall network proof deadline in seconds. Uses the SP1 SDK default if omitted.
    #[arg(
        long,
        env = "SP1_PROOF_TIMEOUT_SECONDS",
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    sp1_proof_timeout_seconds: Option<u64>,

    /// Maximum number of jobs proved concurrently. One suits a local CPU prover; raise it for
    /// the Succinct proving network.
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

    #[arg(long, default_value_t = DEFAULT_WORKER_HEARTBEAT_INTERVAL_SEC)]
    /// The worker heartbeat interval in seconds.
    heartbeat_interval_sec: u64,

    #[arg(long, default_value_t = DEFAULT_WORKER_MAX_CONSECUTIVE_HEARTBEAT_FAILURES)]
    /// Maximum consecutive retryable heartbeat failures before aborting proof generation.
    heartbeat_max_consecutive_failures: u32,
}

pub async fn run(cli: WorkerArgs) -> Result<()> {
    // The worker enables both rustls crypto backends transitively: AWS clients select AWS-LC,
    // while SP1 and other HTTP clients select Ring. Rustls cannot infer a default when both are
    // present, so install one before SP1 constructs its tonic TLS client.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let spec = cli.network.chain_spec();
    let schedule = hardfork_config_from_chain_spec(spec.as_ref());
    let host = build_online_config(
        cli.rollup_config.clone(),
        cli.rollup_config_hash,
        cli.l1_rpc.clone(),
        cli.l1_beacon_rpc.clone(),
        cli.l2_rpc.clone(),
        cli.l2_consensus_rpc.clone(),
        cli.network.chain_id(),
        &schedule,
        Duration::from_secs(cli.witness_timeout_seconds),
    )?;

    // ELFs are embedded at compile time via `sp1_sdk::include_elf!()`
    // (see `proofs/backends/sp1/elfs/build.rs`). Challenged roots are
    // defended on-chain; Groth16 keeps verification ~100k gas.
    let prover_kind = cli.prover;
    match prover_kind {
        Sp1ProverKind::Cpu => {
            run_worker(
                &cli,
                host,
                CpuSuccinctProver::new(SP1ProofMode::Groth16).await?,
            )
            .await
        }
        Sp1ProverKind::Mock => {
            run_worker(
                &cli,
                host,
                MockSuccinctProver::new(SP1ProofMode::Groth16).await?,
            )
            .await
        }
        Sp1ProverKind::Network => {
            let l1_rpc_url = cli
                .sp1_network_l1_rpc_url
                .as_ref()
                .context("SP1_NETWORK_L1_RPC_URL is required when --prover network")?;
            let (network_secret, signer_type) = select_network_signer(
                cli.sp1_private_key.as_deref(),
                cli.sp1_kms_key_id.as_deref(),
            );
            let vapp_address = cli
                .succinct_vapp_address
                .context("SUCCINCT_VAPP_ADDRESS is required when --prover network")?;
            let settlement = load_settlement_config(l1_rpc_url, vapp_address)
                .await
                .context("validating Succinct settlement configuration")?;
            let minimum_balance = parse_prove_amount(&cli.sp1_network_minimum_balance)
                .context("invalid SP1 Network minimum balance")?;
            let refill_amount = cli
                .sp1_network_refill_amount
                .as_deref()
                .map(parse_prove_amount)
                .transpose()
                .context("invalid SP1 Network refill amount")?;
            let refill_amount =
                resolve_refill_amount(refill_amount, settlement.min_deposit_amount)?;
            let refill = NetworkRefillConfig {
                l1_rpc_url: l1_rpc_url.clone(),
                settlement,
                signer_secret: network_secret.to_owned(),
                signer_type,
                refill_amount,
            };
            let connection = NetworkConnection::new(network_secret, signer_type).await?;
            let credit_client = NetworkCreditClient::from_connection(connection.clone());

            if !wait_for_sufficient_network_balance(&credit_client, minimum_balance, &refill)
                .await?
            {
                return Ok(());
            }

            monitor_network_balance(credit_client, minimum_balance, refill);
            let limits = (!cli.sp1_estimate_limits).then_some(NetworkProverLimits {
                range: ProofLimits {
                    cycle_limit: cli.sp1_range_cycle_limit,
                    gas_limit: cli.sp1_range_gas_limit,
                },
                aggregation: ProofLimits {
                    cycle_limit: cli.sp1_aggregation_cycle_limit,
                    gas_limit: cli.sp1_aggregation_gas_limit,
                },
            });
            run_worker(
                &cli,
                host,
                NetworkSuccinctProver::from_connection_with_network_config(
                    SP1ProofMode::Groth16,
                    connection,
                    NetworkProofRequestConfig {
                        limits,
                        max_price_per_pgu: cli.sp1_max_price_per_pgu,
                        auction_timeout: cli.sp1_auction_timeout_seconds.map(Duration::from_secs),
                        proof_timeout: cli.sp1_proof_timeout_seconds.map(Duration::from_secs),
                    },
                )
                .await?,
            )
            .await
        }
    }
}

struct NetworkRefillConfig {
    l1_rpc_url: String,
    settlement: SettlementConfig,
    signer_secret: String,
    signer_type: SignerType,
    refill_amount: U256,
}

async fn wait_for_sufficient_network_balance(
    client: &NetworkCreditClient,
    minimum_balance: U256,
    refill: &NetworkRefillConfig,
) -> Result<bool> {
    let mut pending_deposit = None;
    loop {
        if maintain_network_balance(client, minimum_balance, refill, &mut pending_deposit).await {
            tracing::info!(
                minimum_balance = %format_prove(minimum_balance),
                "SP1 Network credit balance is sufficient"
            );
            return Ok(true);
        }

        tokio::select! {
            () = tokio::time::sleep(SP1_NETWORK_BALANCE_POLL_INTERVAL) => {}
            result = tokio::signal::ctrl_c() => {
                result.context("installing ctrl-c handler while waiting for SP1 Network credit")?;
                tracing::info!("received ctrl-c while waiting for SP1 Network credit");
                return Ok(false);
            }
        }
    }
}

fn monitor_network_balance(
    client: NetworkCreditClient,
    minimum_balance: U256,
    refill: NetworkRefillConfig,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(SP1_NETWORK_BALANCE_POLL_INTERVAL);
        let mut pending_deposit = None;
        interval.tick().await;
        loop {
            interval.tick().await;
            maintain_network_balance(&client, minimum_balance, &refill, &mut pending_deposit).await;
        }
    });
}

async fn maintain_network_balance(
    client: &NetworkCreditClient,
    minimum_balance: U256,
    refill: &NetworkRefillConfig,
    pending_deposit: &mut Option<DepositReceipt>,
) -> bool {
    let balance = match client.get_balance().await {
        Ok(balance) => balance,
        Err(error) => {
            world_chain_proof_metrics::record_sp1_network_balance_unavailable();
            tracing::warn!(%error, "failed to refresh SP1 Network credit balance");
            return false;
        }
    };
    if record_network_balance(balance, minimum_balance) {
        if let Some(receipt) = pending_deposit.take() {
            tracing::info!(
                tx_hash = %receipt.tx_hash,
                onchain_receipt = receipt.onchain_receipt,
                balance = %format_prove(balance),
                "SP1 Network refill is reflected in credits"
            );
        }
        return true;
    }

    if let Some(receipt) = pending_deposit {
        tracing::warn!(
            tx_hash = %receipt.tx_hash,
            onchain_receipt = receipt.onchain_receipt,
            balance = %format_prove(balance),
            minimum_balance = %format_prove(minimum_balance),
            "waiting for confirmed SP1 Network refill to be reflected in credits"
        );
        return false;
    }

    let amount = refill_amount_for_balance(balance, minimum_balance, refill.refill_amount)
        .expect("insufficient balance must have a refill amount");
    tracing::warn!(
        balance = %format_prove(balance),
        minimum_balance = %format_prove(minimum_balance),
        refill_amount = %format_prove(amount),
        "SP1 Network credit balance is too low; submitting refill"
    );
    match submit_deposit(
        &refill.l1_rpc_url,
        refill.settlement,
        &refill.signer_secret,
        refill.signer_type,
        amount,
    )
    .await
    {
        Ok(receipt) => {
            tracing::info!(
                tx_hash = %receipt.tx_hash,
                onchain_receipt = receipt.onchain_receipt,
                refill_amount = %format_prove(amount),
                "SP1 Network refill confirmed on Ethereum mainnet"
            );
            *pending_deposit = Some(receipt);
        }
        Err(error) => {
            tracing::error!(%error, "failed to refill SP1 Network credits");
        }
    }
    false
}

fn refill_amount_for_balance(
    balance: U256,
    minimum_balance: U256,
    refill_amount: U256,
) -> Option<U256> {
    if balance >= minimum_balance {
        return None;
    }
    let shortfall = minimum_balance.checked_sub(balance)?;
    Some(shortfall.max(refill_amount))
}

fn resolve_refill_amount(configured: Option<U256>, min_deposit_amount: U256) -> Result<U256> {
    let amount = configured.unwrap_or(min_deposit_amount);
    if amount < min_deposit_amount {
        anyhow::bail!(
            "SP1 Network refill amount {} PROVE is below SuccinctVApp.minDepositAmount() of {} PROVE",
            format_prove(amount),
            format_prove(min_deposit_amount),
        );
    }
    Ok(amount)
}

fn record_network_balance(balance: U256, minimum_balance: U256) -> bool {
    let sufficient = balance >= minimum_balance;
    match prove_as_f64(balance) {
        Ok(balance_prove) => {
            world_chain_proof_metrics::record_sp1_network_balance(balance_prove, sufficient);
        }
        Err(error) => {
            world_chain_proof_metrics::record_sp1_network_balance_unavailable();
            tracing::warn!(%error, ?balance, "failed to convert SP1 Network credit balance");
        }
    }
    sufficient
}

async fn run_worker<P>(cli: &WorkerArgs, host: OnlineHostConfig, prover: P) -> Result<()>
where
    P: WorldSuccinctProver + Send + Sync + 'static,
{
    let embedded_vkeys = embedded_vkey_manifest()
        .await
        .context("computing embedded SP1 verifier identifiers")?;
    let l1_rpc_url = cli.l1_rpc.parse().context("invalid L1 RPC URL")?;
    let game_provider =
        AlloyProofGameProvider::new(ProviderBuilder::new().connect_http(l1_rpc_url));
    let backend = Sp1Backend::new(
        host,
        prover,
        game_provider,
        Sp1BackendConfig {
            allow_unfinalized: cli.allow_unfinalized,
            aggregation_vkey: embedded_vkeys.aggregation_vkey,
            range_vkey_commitment: embedded_vkeys.range_vkey_commitment,
        },
    );

    let queue = RpcProverServiceClient::new(&cli.prover_service_url)
        .with_context(|| format!("failed to connect to {}", cli.prover_service_url))?;
    let worker_id = format!("{}-sp1-worker", cli.worker_id);
    let retry_initial_delay = Duration::from_millis(cli.submit_proof_retry_initial_delay_ms);
    let retry_max_delay = Duration::from_millis(cli.submit_proof_retry_max_delay_ms);
    let retry_config = RetryConfig::new(
        cli.submit_proof_retry_max_retries,
        retry_initial_delay,
        retry_max_delay,
    );
    let heartbeat_config = WorkerHeartbeatConfig::with_max_consecutive_failures(
        Duration::from_secs(cli.heartbeat_interval_sec),
        cli.heartbeat_max_consecutive_failures,
    );
    let worker = ProofWorker::new(
        queue,
        backend,
        ProofWorkerConfig {
            worker_id,
            poll_interval: Duration::from_secs(cli.poll_interval_seconds),
            max_concurrent_jobs: cli.max_concurrent_jobs,
            retry_config,
            heartbeat_config,
        },
    );

    tracing::info!(
        prover_service = %cli.prover_service_url,
        prover = %cli.prover,
        sp1_estimate_limits = cli.sp1_estimate_limits,
        sp1_range_cycle_limit = cli.sp1_range_cycle_limit,
        sp1_range_gas_limit = cli.sp1_range_gas_limit,
        sp1_aggregation_cycle_limit = cli.sp1_aggregation_cycle_limit,
        sp1_aggregation_gas_limit = cli.sp1_aggregation_gas_limit,
        sp1_max_price_per_pgu = ?cli.sp1_max_price_per_pgu,
        sp1_auction_timeout_seconds = ?cli.sp1_auction_timeout_seconds,
        sp1_proof_timeout_seconds = ?cli.sp1_proof_timeout_seconds,
        aggregation_vkey = %embedded_vkeys.aggregation_vkey,
        range_vkey_commitment = %embedded_vkeys.range_vkey_commitment,
        submit_proof_retry_max_retries = cli.submit_proof_retry_max_retries,
        submit_proof_retry_initial_delay_ms = cli.submit_proof_retry_initial_delay_ms,
        submit_proof_retry_max_delay_ms = cli.submit_proof_retry_max_delay_ms,
        "sp1-worker starting"
    );

    // Ctrl-C triggers a graceful shutdown: the worker stops leasing, flushes pending
    // reports, and resolves.
    let token = worker.cancellation_token();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("received ctrl-c, shutting down");
            token.cancel();
        }
    });

    worker.await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_args() -> Vec<&'static str> {
        vec![
            "sp1-worker",
            "--prover-service-url",
            "http://127.0.0.1:8545",
            "--l2-rpc",
            "http://127.0.0.1:9545",
            "--l1-rpc",
            "http://127.0.0.1:8545",
            "--l1-beacon-rpc",
            "http://127.0.0.1:5052",
            "--worker-id",
            "test",
            "--sp1-kms-key-id",
            "alias/prover",
        ]
    }

    #[test]
    fn uses_optimistic_sp1_limits_by_default() {
        let cli = WorkerArgs::parse_from(base_args());

        assert!(!cli.sp1_estimate_limits);
        assert_eq!(cli.sp1_range_cycle_limit, DEFAULT_SP1_RANGE_CYCLE_LIMIT);
        assert_eq!(cli.sp1_range_gas_limit, DEFAULT_SP1_RANGE_GAS_LIMIT);
        assert_eq!(
            cli.sp1_aggregation_cycle_limit,
            DEFAULT_SP1_AGGREGATION_CYCLE_LIMIT
        );
        assert_eq!(
            cli.sp1_aggregation_gas_limit,
            DEFAULT_SP1_AGGREGATION_GAS_LIMIT
        );
        assert_eq!(cli.sp1_max_price_per_pgu, None);
        assert_eq!(cli.sp1_auction_timeout_seconds, None);
        assert_eq!(cli.sp1_proof_timeout_seconds, None);
    }

    #[test]
    fn local_limit_estimation_conflicts_with_explicit_limits() {
        let mut args = base_args();
        args.extend(["--sp1-estimate-limits", "--sp1-range-cycle-limit", "1000"]);

        let error = WorkerArgs::try_parse_from(args)
            .expect_err("local estimation and explicit limits should conflict");

        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn local_limit_estimation_can_use_defaulted_limit_arguments() {
        let mut args = base_args();
        args.push("--sp1-estimate-limits");

        let cli = WorkerArgs::parse_from(args);

        assert!(cli.sp1_estimate_limits);
    }

    #[test]
    fn worker_rejects_multiple_signers() {
        let mut multiple = base_args();
        multiple.extend(["--sp1-private-key", "0x1234"]);
        assert_eq!(
            WorkerArgs::try_parse_from(multiple).unwrap_err().kind(),
            clap::error::ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn parses_sp1_max_price_per_pgu() {
        let mut args = base_args();
        args.extend(["--sp1-max-price-per-pgu", "50000000"]);

        let cli = WorkerArgs::parse_from(args);

        assert_eq!(cli.sp1_max_price_per_pgu, Some(50_000_000));
    }

    #[test]
    fn rejects_zero_sp1_max_price_per_pgu() {
        let mut args = base_args();
        args.extend(["--sp1-max-price-per-pgu", "0"]);

        let error = WorkerArgs::try_parse_from(args)
            .expect_err("zero max price per PGU should be rejected");

        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn parses_sp1_network_timeouts() {
        let mut args = base_args();
        args.extend([
            "--sp1-auction-timeout-seconds",
            "120",
            "--sp1-proof-timeout-seconds",
            "28800",
        ]);

        let cli = WorkerArgs::parse_from(args);

        assert_eq!(cli.sp1_auction_timeout_seconds, Some(120));
        assert_eq!(cli.sp1_proof_timeout_seconds, Some(28_800));
    }

    #[test]
    fn rejects_zero_sp1_network_timeouts() {
        for argument in [
            "--sp1-auction-timeout-seconds",
            "--sp1-proof-timeout-seconds",
        ] {
            let mut args = base_args();
            args.extend([argument, "0"]);

            let error = WorkerArgs::try_parse_from(args)
                .expect_err("zero SP1 Network timeout should be rejected");

            assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
        }
    }

    #[test]
    fn defaults_to_ten_prove_minimum_balance() {
        let cli = WorkerArgs::parse_from(base_args());

        assert_eq!(
            cli.sp1_network_minimum_balance,
            DEFAULT_SP1_NETWORK_MINIMUM_BALANCE
        );
        assert_eq!(cli.sp1_network_refill_amount, None);
    }

    #[test]
    fn parses_minimum_balance_as_prove() {
        assert_eq!(
            parse_prove_amount(DEFAULT_SP1_NETWORK_MINIMUM_BALANCE).unwrap(),
            U256::from(10_000_000_000_000_000_000_u128)
        );
    }

    #[test]
    fn refill_covers_the_shortfall_or_configured_chunk() {
        assert_eq!(
            refill_amount_for_balance(U256::from(2), U256::from(10), U256::from(3)),
            Some(U256::from(8))
        );
        assert_eq!(
            refill_amount_for_balance(U256::from(9), U256::from(10), U256::from(3)),
            Some(U256::from(3))
        );
        assert_eq!(
            refill_amount_for_balance(U256::from(10), U256::from(10), U256::from(3)),
            None
        );
    }

    #[test]
    fn refill_defaults_to_vapp_minimum_and_rejects_smaller_values() {
        let vapp_minimum = U256::from(10_000_u64);

        assert_eq!(
            resolve_refill_amount(None, vapp_minimum).unwrap(),
            vapp_minimum
        );
        assert!(resolve_refill_amount(Some(U256::from(9_999)), vapp_minimum).is_err());
    }
}
