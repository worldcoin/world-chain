mod cmd;

use clap::{Parser, Subcommand};
use cmd::get_attestation::GetAttestationArgs;
use cmd::run::WorkerArgs;

#[derive(Parser)]
#[command(name = "nitro-worker", about = "World Chain Nitro TEE proving worker")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the proving worker.
    Run(WorkerArgs),
    /// Fetch a bare attestation document from the running enclave and print hex to stdout.
    GetAttestation(GetAttestationArgs),
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("nitro-worker requires Linux (AF_VSOCK)");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    match Cli::parse().command {
        Command::Run(args) => cmd::run::run(args).await?,
        Command::GetAttestation(args) => cmd::get_attestation::get_attestation(args).await?,
    }
    Ok(())
}
