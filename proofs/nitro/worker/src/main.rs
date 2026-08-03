#[cfg(target_os = "linux")]
mod cmd;

#[cfg(target_os = "linux")]
use clap::{Parser, Subcommand};
#[cfg(target_os = "linux")]
use cmd::{get_attestation::GetAttestationArgs, register::RegisterArgs, run::WorkerArgs};
#[cfg(target_os = "linux")]
#[derive(Parser)]
#[command(name = "nitro-worker", about = "World Chain Nitro TEE proving worker")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[cfg(target_os = "linux")]
#[derive(Subcommand)]
enum Command {
    /// Start the proving worker.
    Run(Box<WorkerArgs>),
    /// Fetch a bare attestation document from the running enclave and print hex to stdout.
    GetAttestation(GetAttestationArgs),
    /// Register the enclave's generated signing key on-chain via `registerKey`.
    Register(RegisterArgs),
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
    let _telemetry_guard = telemetry_batteries::init()
        .map_err(|error| anyhow::anyhow!("failed to initialize telemetry: {error:#}"))?;

    match Cli::parse().command {
        Command::Run(args) => cmd::run::run(*args).await?,
        Command::GetAttestation(args) => cmd::get_attestation::get_attestation(args).await?,
        Command::Register(args) => cmd::register::register(args).await?,
    }
    Ok(())
}
