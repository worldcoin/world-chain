#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("nitro-worker requires Linux (AF_VSOCK)");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
use world_chain_nitro_worker::WorkerArgs;

#[cfg(target_os = "linux")]
#[derive(clap::Parser)]
#[command(name = "nitro-worker", about = "World Chain Nitro TEE proving worker")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[cfg(target_os = "linux")]
#[derive(clap::Subcommand)]
enum Command {
    /// Start the proving worker.
    Run(WorkerArgs),
    /// Fetch a bare attestation document from the running enclave and print hex to stdout.
    GetAttestation(GetAttestationArgs),
}

/// Enclave connection arguments for the get-attestation subcommand.
#[cfg(target_os = "linux")]
#[derive(clap::Args)]
struct CommonArgs {
    /// vsock CID of the running Nitro Enclave.
    #[arg(long, env = "ENCLAVE_CID", default_value_t = 16)]
    enclave_cid: u32,

    /// vsock port the enclave listens on.
    #[arg(
        long,
        env = "ENCLAVE_PORT",
        default_value_t = world_chain_proof_nitro::protocol::DEFAULT_VSOCK_PORT
    )]
    enclave_port: u32,
}

#[cfg(target_os = "linux")]
#[derive(clap::Args)]
struct GetAttestationArgs {
    #[command(flatten)]
    common: CommonArgs,
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() {
    use clap::Parser;

    let cli = Cli::parse();

    match cli.command {
        Command::Run(args) => {
            if let Err(e) = world_chain_nitro_worker::run_with_cli(args).await {
                eprintln!("error: {e:#}");
                std::process::exit(1);
            }
        }
        Command::GetAttestation(args) => {
            if let Err(e) = get_attestation(args).await {
                eprintln!("error: {e:#}");
                std::process::exit(1);
            }
        }
    }
}

#[cfg(target_os = "linux")]
async fn get_attestation(args: GetAttestationArgs) -> anyhow::Result<()> {
    use world_chain_proof_nitro::{
        ExpectedPcrs,
        host::{EnclaveEndpoint, NitroProver},
    };

    let prover = NitroProver::new(
        EnclaveEndpoint::with_port(args.common.enclave_cid, args.common.enclave_port),
        ExpectedPcrs::PLACEHOLDER,
    );

    let attestation_doc = prover
        .get_attestation()
        .await
        .map_err(|e| anyhow::anyhow!("get_attestation failed: {e}"))?;

    println!("{}", hex::encode(attestation_doc));
    Ok(())
}
