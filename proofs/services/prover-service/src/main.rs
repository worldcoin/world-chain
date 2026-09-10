//! `world-chain-prover-service` binary: hosts the proof-request JSON-RPC queue that sits
//! between the defender (which requests proofs) and the proof workers (which lease and prove
//! them).
//!
//! Mirrors the in-process prover-service wired by the devnet harness
//! (`crates/devnet/src/full_stack.rs::start_prover_service`), reading its configuration
//! from flags/environment so it can run as a standalone service.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use alloy_provider::ProviderBuilder;
use anyhow::{Context, Result};
use clap::Parser;
use tracing::info;
use world_chain_prover_service::{
    ProverService, ProverServiceConfig, run_status_poller, start_rpc_server,
};

#[derive(Debug, Parser)]
#[command(
    name = "world-chain-prover-service",
    about = "World Chain proof-request queue: serves proof jobs to SP1 workers over JSON-RPC"
)]
struct Cli {
    /// Ethereum L1 execution RPC URL used to check whether queued proofs are still needed.
    #[arg(long, env = "L1_RPC_URL")]
    l1_rpc: String,

    /// Optional fallback Ethereum L1 execution RPC URL.
    #[arg(long, env = "L1_FALLBACK_RPC_URL")]
    l1_fallback_rpc: Option<String>,

    /// Per-request timeout for L1 RPC calls in seconds.
    #[arg(long, env = "L1_RPC_TIMEOUT_SECONDS", default_value_t = world_chain_proof_metrics::DEFAULT_RPC_REQUEST_TIMEOUT_SECONDS)]
    l1_rpc_timeout_seconds: u64,

    /// Address the JSON-RPC server binds to.
    #[arg(long, env = "LISTEN_ADDR", default_value = "0.0.0.0:8080")]
    listen_addr: SocketAddr,

    /// Postgres connection URL for durable prover-service state.
    #[arg(long, env = "PROVER_SERVICE_DATABASE_URL")]
    database_url: String,

    /// Seconds a worker holds a job lease before it is re-queued.
    #[arg(long, env = "LEASE_TIMEOUT_SECONDS", default_value_t = 1800)]
    lease_timeout_seconds: u64,

    /// Maximum proving attempts (leases) per request before it is failed.
    #[arg(long, env = "MAX_ATTEMPTS", default_value_t = 3)]
    max_attempts: u32,

    /// Maximum proving retries per request before it is failed.
    #[arg(long, env = "MAX_RETRIES", default_value_t = 3)]
    max_retries: u32,

    /// Seconds to wait before polling an unchanged backend job again.
    #[arg(long, env = "BACKEND_POLL_INTERVAL_SECONDS", default_value_t = 30)]
    backend_poll_interval_seconds: u64,

    /// Seconds between scans that cancel obsolete jobs and fail exhausted attempts.
    #[arg(long, env = "STATUS_POLLER_INTERVAL_SECS", default_value_t = 30)]
    status_poller_interval_secs: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let _telemetry_guard = telemetry_batteries::init()
        .map_err(|error| anyhow::anyhow!("failed to initialize telemetry: {error:#}"))?;
    world_chain_proof_metrics::describe_metrics();

    let cli = Cli::parse();
    let l1_fallback_rpc_url = cli
        .l1_fallback_rpc
        .as_deref()
        .map(str::parse)
        .transpose()
        .context("invalid L1 fallback RPC URL")?;
    let l1_rpc_client = world_chain_proof_metrics::metered_http_client(
        cli.l1_rpc.parse().context("invalid L1 RPC URL")?,
        l1_fallback_rpc_url,
        world_chain_proof_metrics::RPC_TARGET_L1_EXECUTION,
        Duration::from_secs(cli.l1_rpc_timeout_seconds),
    )
    .context("failed to build the L1 RPC client")?;
    let provider = ProviderBuilder::new()
        .disable_recommended_fillers()
        .connect_client(l1_rpc_client);

    let config = ProverServiceConfig {
        lock_timeout: Duration::from_secs(cli.lease_timeout_seconds),
        max_attempts: cli.max_attempts,
        backend_poll_interval: Duration::from_secs(cli.backend_poll_interval_seconds),
        max_retries: cli.max_retries,
        status_poller_interval: Duration::from_secs(cli.status_poller_interval_secs),
    };
    let status_poller_interval = config.status_poller_interval;
    let service = Arc::new(
        ProverService::connect(&cli.database_url, config)
            .await
            .context("failed to initialize postgres-backed prover-service")?,
    );
    let (addr, handle) = start_rpc_server(cli.listen_addr, Arc::clone(&service))
        .await
        .context("failed to start prover-service RPC server")?;

    let status_poller = tokio::spawn(run_status_poller(service, provider, status_poller_interval));

    info!(
        listen_addr = %addr,
        l1_fallback_rpc_configured = cli.l1_fallback_rpc.is_some(),
        l1_rpc_timeout_seconds = cli.l1_rpc_timeout_seconds,
        "world-chain prover-service started"
    );

    tokio::select! {
        _ = handle.clone().stopped() => info!("prover-service RPC server stopped"),
        _ = tokio::signal::ctrl_c() => {
            info!("received ctrl-c, shutting down");
            let _ = handle.stop();
        }
    }
    status_poller.abort();
    Ok(())
}
