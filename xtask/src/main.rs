use std::{fs::OpenOptions, path::PathBuf};

use clap::Parser;
use eyre::eyre::Context;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod devnet;
mod docs;
mod preflight;
mod stress;
mod swarm;
mod toolkit;

#[derive(Parser)]
#[command(name = "xtask", about = "World Chain development tasks")]
enum Command {
    /// Generate CLI reference documentation for the mdbook
    Docs(docs::Args),
    /// Manage the native Rust World Chain devnet
    Devnet(devnet::Args),
    /// Run preflight checks (auto-fix + verify)
    Preflight(preflight::Args),
    /// Launch a local node swarm
    LaunchNode(swarm::Args),
    /// Run stress tests against a live network
    Stress(stress::Args),
    /// Prove a PBH transaction
    Prove(toolkit::Args),
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    dotenvy::dotenv().ok();

    let cmd = Command::parse();

    match cmd {
        Command::Docs(args) => {
            tracing_subscriber::fmt::init();
            docs::run(args)
        }
        Command::Devnet(args) => {
            let _log_guard = init_devnet_tracing()?;

            devnet::run(args).await
        }
        Command::Preflight(args) => preflight::run(args),
        Command::LaunchNode(args) => {
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .init();

            swarm::run(args).await
        }
        Command::Stress(args) => {
            tracing_subscriber::fmt::init();
            stress::run(args).await
        }
        Command::Prove(args) => toolkit::run(args).await,
    }
}

fn init_devnet_tracing() -> eyre::Result<tracing_appender::non_blocking::WorkerGuard> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let log_path = devnet_log_path();
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .wrap_err_with(|| format!("failed to open {}", log_path.display()))?;
    let (file_writer, guard) = tracing_appender::non_blocking(file);

    let stdout_layer = tracing_subscriber::fmt::layer()
        .without_time()
        .with_target(false);
    let file_layer = tracing_subscriber::fmt::layer()
        .without_time()
        .with_target(false)
        .with_ansi(false)
        .with_writer(file_writer);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();

    tracing::info!(path = %log_path.display(), "devnet logs file");
    Ok(guard)
}

fn devnet_log_path() -> PathBuf {
    std::env::var_os("WORLD_CHAIN_DEVNET_LOG_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/devnet/logs/devnet.log"))
}
