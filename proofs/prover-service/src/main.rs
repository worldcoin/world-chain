//! `world-chain-prover-service` binary: hosts the proof-request JSON-RPC queue that sits
//! between the defender (which requests proofs) and the SP1 workers (which lease and prove
//! them).
//!
//! Mirrors the in-process prover-service wired by the devnet harness
//! (`crates/devnet/src/full_stack.rs::start_prover_service`), reading its configuration
//! from flags/environment so it can run as a standalone service.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use clap::Parser;
use tracing::info;
use world_chain_prover_service::{ProverService, ProverServiceConfig, start_rpc_server};

#[derive(Debug, Parser)]
#[command(
    name = "world-chain-prover-service",
    about = "World Chain proof-request queue: serves proof jobs to SP1 workers over JSON-RPC"
)]
struct Cli {
    /// Address the JSON-RPC server binds to.
    #[arg(long, env = "LISTEN_ADDR", default_value = "0.0.0.0:8080")]
    listen_addr: SocketAddr,

    /// Seconds a worker holds a job lease before it is re-queued.
    #[arg(long, env = "LEASE_TIMEOUT_SECONDS", default_value_t = 1800)]
    lease_timeout_seconds: u64,

    /// Maximum proving attempts (leases) per request before it is failed.
    #[arg(long, env = "MAX_ATTEMPTS", default_value_t = 3)]
    max_attempts: u32,

    /// Maximum number of requests queued per backend.
    #[arg(long, env = "MAX_QUEUE_LEN", default_value_t = 1024)]
    max_queue_len: usize,

    /// Maximum number of finished jobs retained in memory.
    #[arg(long, env = "MAX_FINISHED_JOBS", default_value_t = 1024)]
    max_finished_jobs: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let config = ProverServiceConfig {
        lease_timeout: Duration::from_secs(cli.lease_timeout_seconds),
        max_attempts: cli.max_attempts,
        max_queue_len: cli.max_queue_len,
        max_finished_jobs: cli.max_finished_jobs,
    };
    let service = Arc::new(ProverService::new(config).context("invalid prover-service config")?);
    let (addr, handle) = start_rpc_server(cli.listen_addr, service)
        .await
        .context("failed to start prover-service RPC server")?;

    info!(listen_addr = %addr, "world-chain prover-service started");

    tokio::select! {
        _ = handle.clone().stopped() => info!("prover-service RPC server stopped"),
        _ = tokio::signal::ctrl_c() => {
            info!("received ctrl-c, shutting down");
            let _ = handle.stop();
        }
    }
    Ok(())
}
