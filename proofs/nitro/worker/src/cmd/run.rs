#![cfg(target_os = "linux")]

use std::{path::PathBuf, sync::Arc, time::Duration};

use alloy_primitives::B256;
use anyhow::{Context, Result};
use backon::{ExponentialBuilder, Retryable};
use clap::Parser;
use tracing::{error, info};
use world_chain_chainspec::WorldChainSpec;
use world_chain_nitro_worker::{
    EnclaveBinding, EnclaveCidSource, NitroBackend, NitroBackendConfig, RegistrationCredentials,
    build_expected_pcrs,
};
use world_chain_proof_kona_host_utils::online::{
    build_online_config, hardfork_config_from_chain_spec,
};
use world_chain_proof_nitro::register::RegistrationOutcome;
use world_chain_proof_worker::{
    ProofWorker, ProofWorkerConfig, RetryConfig, WorkerHeartbeatConfig,
};
use world_chain_prover_service::RpcProverServiceClient;

const DEFAULT_SUBMIT_PROOF_RETRY_MAX_RETRIES: usize = 10;
const DEFAULT_SUBMIT_PROOF_RETRY_INITIAL_DELAY_MS: u64 = 100;
const DEFAULT_SUBMIT_PROOF_RETRY_MAX_DELAY_MS: u64 = 10_000;
const DEFAULT_WORKER_HEARTBEAT_INTERVAL_SEC: u64 = 30;
const DEFAULT_WORKER_MAX_CONSECUTIVE_HEARTBEAT_FAILURES: u32 = 5;

/// First backoff interval between enclave key registration attempts.
const REGISTER_RETRY_INITIAL_DELAY: Duration = Duration::from_secs(5);
/// Ceiling for the registration backoff. Registration can block on a human (approving a PCR
/// set, funding the key), so the ceiling is minutes rather than seconds.
const REGISTER_RETRY_MAX_DELAY: Duration = Duration::from_secs(300);

/// Backoff for enclave key registration: exponential, jittered, and **unbounded**.
///
/// Jitter matters because worker replicas share one funding key — un-jittered retries would
/// collide on the same nonce every interval and keep failing as a group.
fn registration_backoff() -> ExponentialBuilder {
    ExponentialBuilder::default()
        .with_min_delay(REGISTER_RETRY_INITIAL_DELAY)
        .with_max_delay(REGISTER_RETRY_MAX_DELAY)
        .with_jitter()
        .without_max_times()
}

/// Registers the enclave key on-chain, retrying until it succeeds or shutdown is requested.
/// Returns `false` only when shutdown won.
///
/// This deliberately never aborts the process. Every way registration can fail — PCR set not
/// yet approved on-chain, registration key unfunded, L1 unreachable, certificate chain not yet
/// verifiable — is a condition an operator resolves *while the worker is running*.
async fn bind_with_retry(binding: &EnclaveBinding) -> bool {
    let attempt = || async { binding.bind().await };

    let bind = attempt
        .retry(registration_backoff())
        .notify(|error, delay| {
            error!(
                ?error,
                retry_in_secs = delay.as_secs(),
                "enclave binding failed; worker stays up and will retry without \
             leasing proof jobs"
            );
        });

    // Race the (unbounded) retry against shutdown so a pod being rolled does not have to wait
    // out a full backoff interval.
    tokio::select! {
        outcome = bind => match outcome {
            Ok(session) => {
                match session.registration {
                    Some(RegistrationOutcome::AlreadyRegistered) => {
                        info!(enclave_cid = session.cid, "enclave key already registered on-chain");
                    }
                    Some(RegistrationOutcome::Registered { tx_hash }) => {
                        info!(%tx_hash, enclave_cid = session.cid, "enclave key registered on-chain");
                    }
                    None => info!(enclave_cid = session.cid, "bound to enclave"),
                }
                true
            }
            // Unreachable while the backoff is unbounded, but treat it the same as shutdown
            // rather than leasing jobs against an enclave we could not bind to.
            Err(error) => {
                error!(?error, "enclave binding gave up; not leasing proof jobs");
                false
            }
        },
        _ = tokio::signal::ctrl_c() => {
            info!("received ctrl-c while retrying enclave binding, shutting down");
            false
        }
    }
}

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
    about = "World Chain Nitro TEE proving worker: leases jobs from the prover-service, \
             proves them in a Nitro Enclave, and submits the signed attestations back."
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

    /// World Chain network.
    #[arg(long, env = "NETWORK", default_value = "worldchain")]
    network: Network,

    /// Rollup config JSON file. If omitted, uses the built-in network config.
    #[arg(long, env = "ROLLUP_CONFIG")]
    rollup_config: Option<PathBuf>,

    /// Rollup config hash override (required when --rollup-config is not supplied).
    #[arg(long, env = "ROLLUP_CONFIG_HASH")]
    rollup_config_hash: Option<B256>,

    /// L2 blocks between a proposal's parent and its claimed block (the proof system's
    /// `blockInterval` domain constant).
    #[arg(long, env = "BLOCK_INTERVAL")]
    block_interval: u64,

    /// vsock CID of the running Nitro Enclave. Ignored when `--enclave-cid-file` is set.
    #[arg(long, env = "ENCLAVE_CID", default_value_t = 16)]
    enclave_cid: u32,

    /// File the enclave launcher rewrites with the current vsock CID. Prefer this over
    /// `--enclave-cid`: it is re-read before every job, so an enclave replaced underneath the
    /// worker is picked up instead of stranding it on a dead CID.
    #[arg(long, env = "ENCLAVE_CID_FILE")]
    enclave_cid_file: Option<PathBuf>,

    /// vsock port the enclave listens on.
    #[arg(
        long,
        env = "ENCLAVE_PORT",
        default_value_t = world_chain_proof_nitro::protocol::DEFAULT_VSOCK_PORT
    )]
    enclave_port: u32,

    /// PCR0 hex (48 bytes). All three PCRs must be provided for production use.
    #[arg(long, env = "PCR0")]
    pcr0: Option<String>,

    /// PCR1 hex (48 bytes).
    #[arg(long, env = "PCR1")]
    pcr1: Option<String>,

    /// PCR2 hex (48 bytes).
    #[arg(long, env = "PCR2")]
    pcr2: Option<String>,

    /// Seconds to sleep between job-queue polls when no work is available.
    #[arg(long, env = "POLL_INTERVAL_SECONDS", default_value_t = 10)]
    poll_interval_seconds: u64,

    /// Maximum seconds to spend generating one Kona witness.
    #[arg(long, default_value_t = 900)]
    witness_timeout_seconds: u64,

    /// Maximum number of jobs proved concurrently. TEE attestation is cheaper than ZK
    /// proving, so this can be higher than for SP1 workers.
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

    /// The worker heartbeat interval in seconds.
    #[arg(long, default_value_t = DEFAULT_WORKER_HEARTBEAT_INTERVAL_SEC)]
    heartbeat_interval_sec: u64,

    /// Maximum consecutive retryable heartbeat failures before aborting proof generation.
    #[arg(long, default_value_t = DEFAULT_WORKER_MAX_CONSECUTIVE_HEARTBEAT_FAILURES)]
    heartbeat_max_consecutive_failures: u32,

    /// Register the enclave's generated signing key on-chain at startup before leasing
    /// jobs. Idempotent: if the key is already registered the worker continues normally.
    #[arg(long, env = "AUTO_REGISTER", default_value_t = false)]
    auto_register: bool,

    /// `NitroEnclaveKeyRegistry` contract address on L1. Required when `--auto-register`
    /// is set.
    #[arg(long, env = "NITRO_ENCLAVE_KEY_REGISTRY")]
    registry: Option<String>,

    /// Hex-encoded private key used to sign and pay for the `registerKey` transaction when
    /// `--auto-register` is set. Falls back to `PRIVATE_KEY` when unset. `registerKey` is
    /// not owner-gated, so any funded key works.
    #[arg(long, env = "REGISTER_PRIVATE_KEY", hide_env_values = true)]
    register_private_key: Option<String>,
}

pub async fn run(args: WorkerArgs) -> Result<()> {
    let spec = args.network.chain_spec();
    let schedule = hardfork_config_from_chain_spec(spec.as_ref());
    let online = build_online_config(
        args.rollup_config.clone(),
        args.rollup_config_hash,
        args.l1_rpc.clone(),
        args.l1_beacon_rpc.clone(),
        args.l2_rpc.clone(),
        args.l2_consensus_rpc.clone(),
        args.network.chain_id(),
        &schedule,
        Duration::from_secs(args.witness_timeout_seconds),
    )?;
    let expected_pcrs = build_expected_pcrs(
        args.pcr0.as_deref(),
        args.pcr1.as_deref(),
        args.pcr2.as_deref(),
    )?;

    let cid_source = args.enclave_cid_file.clone().map_or_else(
        || EnclaveCidSource::Fixed(args.enclave_cid),
        EnclaveCidSource::File,
    );

    let registration = if args.auto_register {
        Some(RegistrationCredentials {
            // Registration reuses the same L1 endpoint as witness building (`--l1-rpc`).
            l1_rpc_url: args.l1_rpc.clone(),
            registry: args
                .registry
                .clone()
                .context("--auto-register requires --registry / NITRO_ENCLAVE_KEY_REGISTRY")?,
            private_key: args
                .register_private_key
                .clone()
                .or_else(|| std::env::var("PRIVATE_KEY").ok())
                .context(
                    "--auto-register requires a key: set --register-private-key, \
                     REGISTER_PRIVATE_KEY, or PRIVATE_KEY",
                )?,
        })
    } else {
        None
    };

    let binding = Arc::new(EnclaveBinding::new(
        cid_source,
        args.enclave_port,
        expected_pcrs,
        registration,
    ));

    // Bind before leasing any jobs: proofs signed by an unregistered key do not verify, so an
    // unbound worker must not take work. Binding is a *precondition*, not a fatal error — see
    // [`bind_with_retry`] for why this never aborts the process. Every job re-binds, so this
    // only gates startup.
    if args.auto_register {
        // Publish the gauge before the first attempt so "never registered" is a visible zero
        // rather than an absent series a threshold monitor would silently ignore.
        world_chain_proof_metrics::set_enclave_key_registered(false);
        info!("auto-register enabled; binding to the enclave before leasing jobs");

        if !bind_with_retry(&binding).await {
            // Shutdown was requested while retrying; exit cleanly rather than starting up.
            return Ok(());
        }
    }

    info!(
        prover_service = %args.prover_service_url,
        block_interval = args.block_interval,
        submit_proof_retry_max_retries = args.submit_proof_retry_max_retries,
        submit_proof_retry_initial_delay_ms = args.submit_proof_retry_initial_delay_ms,
        submit_proof_retry_max_delay_ms = args.submit_proof_retry_max_delay_ms,
        "nitro-worker starting"
    );

    let backend = NitroBackend::new(NitroBackendConfig {
        block_interval: args.block_interval,
        online,
        binding: Arc::clone(&binding),
    });

    let queue = RpcProverServiceClient::new(&args.prover_service_url)
        .with_context(|| format!("failed to connect to {}", args.prover_service_url))?;

    let worker_id = format!("{}-nitro-worker", args.worker_id);
    let retry_config = RetryConfig::new(
        args.submit_proof_retry_max_retries,
        Duration::from_millis(args.submit_proof_retry_initial_delay_ms),
        Duration::from_millis(args.submit_proof_retry_max_delay_ms),
    );
    let heartbeat_config = WorkerHeartbeatConfig::with_max_consecutive_failures(
        Duration::from_secs(args.heartbeat_interval_sec),
        args.heartbeat_max_consecutive_failures,
    );
    let worker = ProofWorker::new(
        queue,
        backend,
        ProofWorkerConfig {
            worker_id,
            poll_interval: Duration::from_secs(args.poll_interval_seconds),
            max_concurrent_jobs: args.max_concurrent_jobs,
            retry_config,
            heartbeat_config,
        },
    );

    // Ctrl-C triggers a graceful shutdown: the worker stops leasing, signals the backend to
    // shut down, and resolves.
    let token = worker.cancellation_token();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            info!("received ctrl-c, shutting down");
            token.cancel();
        }
    });

    worker.await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use backon::BackoffBuilder;

    /// The registration backoff must be unbounded: the whole point of this change is that a
    /// worker keeps retrying (and stays exec-able) instead of crashlooping. A `with_max_times`
    /// creeping in here would silently restore the give-up behaviour.
    #[test]
    fn registration_backoff_never_gives_up() {
        let mut backoff = registration_backoff().build();
        // Far more attempts than any bounded policy would allow.
        for i in 0..10_000 {
            assert!(
                backoff.next().is_some(),
                "backoff stopped yielding delays at attempt {i}"
            );
        }
    }

    /// Delays must stay within the configured ceiling so a stuck worker retries on a
    /// predictable cadence rather than backing off unboundedly.
    #[test]
    fn registration_backoff_respects_the_delay_ceiling() {
        let mut backoff = registration_backoff().build();
        for _ in 0..256 {
            let delay = backoff.next().expect("unbounded backoff yields a delay");
            // `with_jitter` only ever adds to the base delay, bounded by the base itself.
            assert!(
                delay <= REGISTER_RETRY_MAX_DELAY * 2,
                "delay {delay:?} exceeded the jittered ceiling"
            );
        }
    }
}
