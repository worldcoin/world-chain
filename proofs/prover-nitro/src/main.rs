use std::path::PathBuf;

#[cfg(target_os = "linux")]
use anyhow::Context;
use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};
use world_chain_prover::{
    HashRollupConfigArgs, RpcArgs, WitnessArgs, print_rollup_config_hash, write_witness,
};

#[derive(Debug, Parser)]
#[command(
    name = "world-chain-prover-nitro",
    about = "World Chain AWS Nitro TEE prover"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the rollup config hash used in proofs.
    HashRollupConfig(HashRollupConfigArgs),
    /// Build and write the witness to a file without proving.
    Witness(WitnessArgs),
    /// Generate witness and send to a running Nitro enclave for attested proving.
    Prove(NitroArgs),
    /// Fetch a bare attestation document from a running Nitro enclave.
    ///
    /// This does not run any proof — it simply asks the enclave's NSM device for an
    /// attestation document and prints the raw COSE_Sign1 bytes as hex to stdout.
    /// Useful for CertManager pre-warm workflows. Connects to CID 16 on the default
    /// vsock port. Pipe the output directly into hinted_attestation_calls.js.
    GetAttestation,
}

#[derive(Debug, Args)]
struct NitroArgs {
    #[command(flatten)]
    rpc: RpcArgs,

    /// vsock CID of the running Nitro enclave.
    #[arg(long, env = "ENCLAVE_CID", default_value_t = 16)]
    cid: u32,

    /// vsock port the enclave is listening on.
    #[arg(long, env = "ENCLAVE_PORT", default_value_t = 5005)]
    port: u32,

    /// PCR0 hex (48 bytes).
    #[arg(long, env = "PCR0")]
    pcr0: Option<String>,

    /// PCR1 hex (48 bytes).
    #[arg(long, env = "PCR1")]
    pcr1: Option<String>,

    /// PCR2 hex (48 bytes).
    #[arg(long, env = "PCR2")]
    pcr2: Option<String>,

    /// Output path for the JSON artifact (boot info + attestation doc).
    #[arg(long)]
    output: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    match Cli::parse().command {
        Command::HashRollupConfig(args) => print_rollup_config_hash(args).await?,
        Command::Witness(args) => write_witness(args).await?,
        Command::Prove(args) => nitro_prove(args).await?,
        Command::GetAttestation => get_attestation().await?,
    }

    Ok(())
}

#[cfg(target_os = "linux")]
async fn get_attestation() -> Result<()> {
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

#[cfg(not(target_os = "linux"))]
async fn get_attestation() -> Result<()> {
    bail!("get-attestation requires Linux with AF_VSOCK support")
}

#[cfg(target_os = "linux")]
async fn nitro_prove(args: NitroArgs) -> Result<()> {
    use anyhow::anyhow;
    use world_chain_proof_nitro::{
        ExpectedPcrs, NitroRangeProofRequest,
        attestation::parse_and_check_pcrs,
        host::{EnclaveEndpoint, NitroProver},
        protocol::transition_commitment,
    };
    use world_chain_prover::{build_range_input_from_args, write_json};

    let input = build_range_input_from_args(&args.rpc).await?;

    let expected_pcrs = match (args.pcr0, args.pcr1, args.pcr2) {
        (Some(p0), Some(p1), Some(p2)) => ExpectedPcrs {
            pcr0: hex_to_pcr(&p0)?,
            pcr1: hex_to_pcr(&p1)?,
            pcr2: hex_to_pcr(&p2)?,
        },
        (None, None, None) => {
            bail!(
                "--pcr0/--pcr1/--pcr2 are required: real PCR measurements must be supplied to verify the enclave image"
            );
        }
        _ => bail!("provide all three of --pcr0, --pcr1, --pcr2 or none"),
    };

    let request = NitroRangeProofRequest::from_witness_data(&input.witness, None)
        .map_err(|e| anyhow!("failed to serialize witness: {e}"))?;

    let prover = NitroProver::new(
        EnclaveEndpoint::with_port(args.cid, args.port),
        expected_pcrs,
    );

    println!(
        "sending range {start}..={end} to enclave (cid {cid})",
        start = args.rpc.start_block + 1,
        end = args.rpc.end_block,
        cid = args.cid,
    );

    let artifact = prover
        .prove_range(request)
        .await
        .map_err(|e| anyhow!("enclave proving failed: {e}"))?;

    println!(
        "enclave returned: l2_pre={pre:?} l2_post={post:?} block={block}",
        pre = artifact.transition_public_values.l2PreRoot,
        post = artifact.transition_public_values.l2PostRoot,
        block = artifact.transition_public_values.l2PostBlockNumber,
    );

    let expected_user_data = transition_commitment(&artifact.transition_public_values);
    parse_and_check_pcrs(
        &artifact.attestation_doc,
        &expected_pcrs,
        &expected_user_data,
    )
    .map_err(|e| anyhow!("attestation verification failed: {e}"))?;

    println!("attestation verified OK");
    println!(
        "{}",
        serde_json::to_string_pretty(&artifact.transition_public_values)?
    );

    if let Some(output) = args.output {
        write_json(
            &output,
            &serde_json::json!({
                "transitionPublicValues": artifact.transition_public_values,
                "attestationDoc": format!("0x{}", hex::encode(&artifact.attestation_doc)),
            }),
        )?;
        println!("artifact written to {}", output.display());
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
async fn nitro_prove(_args: NitroArgs) -> Result<()> {
    bail!("world-chain-prover-nitro requires Linux with AF_VSOCK support")
}

#[cfg(target_os = "linux")]
fn hex_to_pcr(hex: &str) -> Result<[u8; 48]> {
    let bytes = hex::decode(hex).context("invalid PCR hex")?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("PCR must be 48 bytes"))
}
