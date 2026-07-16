#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("nitro-worker requires Linux (AF_VSOCK)");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
use world_chain_nitro_worker::Cli as RunArgs;

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
    /// Start the proving worker (default).
    Run(RunArgs),
    /// Fetch a bare attestation doc from the running enclave and print hex to stdout.
    GetAttestation,
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
        Command::GetAttestation => {
            if let Err(e) = get_attestation().await {
                eprintln!("error: {e:#}");
                std::process::exit(1);
            }
        }
    }
}

#[cfg(target_os = "linux")]
async fn get_attestation() -> anyhow::Result<()> {
    use world_chain_proof_nitro::{
        ExpectedPcrs,
        host::{EnclaveEndpoint, NitroProver},
        protocol::DEFAULT_VSOCK_PORT,
    };

    let cid: u32 = match std::env::var("ENCLAVE_CID") {
        Ok(v) => v
            .parse()
            .map_err(|_| anyhow::anyhow!("ENCLAVE_CID is set but not a valid u32: {v:?}"))?,
        Err(_) => 16,
    };

    let prover = NitroProver::new(
        EnclaveEndpoint::with_port(cid, DEFAULT_VSOCK_PORT),
        ExpectedPcrs::PLACEHOLDER,
    );

    let attestation_doc = prover
        .get_attestation()
        .await
        .map_err(|e| anyhow::anyhow!("get_attestation failed: {e}"))?;

    println!("{}", hex::encode(attestation_doc));
    Ok(())
}
