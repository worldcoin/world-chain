use std::{fs, path::PathBuf};

use alloy_primitives::{Address, B256};
use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use serde_json::json;
use world_chain_prover::{
    HashRollupConfigArgs, RpcArgs, WitnessArgs, ensure_parent_dir, online_host_config,
    print_rollup_config_hash, write_json, write_witness,
};

#[derive(Debug, Parser)]
#[command(name = "world-chain-prover-sp1", about = "World Chain SP1 zkVM prover")]
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
    /// Execute the SP1 range program against a witness file (no ZK proof, fast).
    Execute(Sp1ExecuteArgs),
    /// Generate range + aggregation proofs end-to-end from RPC.
    Prove(Box<Sp1ProveArgs>),
    /// Compute the on-chain verification keys for the range and aggregation ELFs.
    Vkeys(Sp1VkeysArgs),
}

#[derive(Debug, Args)]
struct Sp1ExecuteArgs {
    /// rkyv-serialized witness file produced by the `witness` subcommand.
    #[arg(long, env = "WITNESS_PATH")]
    witness: PathBuf,

    /// Path to the SP1 range ELF binary.
    #[arg(long, env = "RANGE_ELF_PATH")]
    elf: PathBuf,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Sp1Mode {
    /// Default. Proof size grows linearly with cycles.
    #[value(name = "core")]
    Core,
    /// Constant-size recursive proof.
    #[value(name = "compressed")]
    Compressed,
    /// PLONK proof, ~300k gas to verify on-chain.
    #[value(name = "plonk")]
    Plonk,
    /// Groth16 proof, ~100k gas to verify on-chain.
    #[value(name = "groth16")]
    Groth16,
}

#[derive(Debug, Args)]
struct Sp1ProveArgs {
    #[command(flatten)]
    rpc: RpcArgs,

    /// Number of equal-length sub-ranges to split the block range into.
    #[arg(long, default_value_t = 1)]
    ranges: u64,

    /// Path to the SP1 range ELF binary.
    #[arg(long, env = "RANGE_ELF_PATH")]
    range_elf: PathBuf,

    /// Path to the SP1 aggregation ELF binary.
    #[arg(long, env = "AGG_ELF_PATH")]
    agg_elf: PathBuf,

    /// Prover backend: cpu, mock, or network. Overrides SP1_PROVER env var.
    #[arg(long, env = "SP1_PROVER", default_value = "cpu")]
    prover: world_chain_proof_succinct_host_utils::env_prover::Sp1ProverKind,

    /// Aggregation proof mode.
    #[arg(long, default_value = "groth16")]
    mode: Sp1Mode,

    /// Prover address for on-chain attribution (defaults to zero address).
    #[arg(long, default_value = "0x0000000000000000000000000000000000000000")]
    prover_address: Address,

    /// Output path for the aggregation proof artifact JSON.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct Sp1VkeysArgs {
    /// Path to the SP1 range ELF binary.
    #[arg(
        long,
        env = "RANGE_ELF_PATH",
        default_value = "proofs/succinct/elf/world-chain-range-ethereum"
    )]
    range_elf: PathBuf,

    /// Path to the SP1 aggregation ELF binary.
    #[arg(
        long,
        env = "AGG_ELF_PATH",
        default_value = "proofs/succinct/elf/world-chain-aggregation"
    )]
    agg_elf: PathBuf,

    /// Output path for the vkeys JSON. Printed to stdout when unset.
    #[arg(long)]
    output: Option<PathBuf>,
}

fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    match Cli::parse().command {
        Command::HashRollupConfig(args) => print_rollup_config_hash(args)?,
        Command::Witness(args) => write_witness(args)?,
        Command::Execute(args) => sp1_execute(args)?,
        Command::Prove(args) => sp1_prove(*args)?,
        Command::Vkeys(args) => sp1_vkeys(args)?,
    }

    Ok(())
}

fn sp1_execute(args: Sp1ExecuteArgs) -> Result<()> {
    use sp1_sdk::{Prover, ProverClient, SP1Stdin};

    let witness_bytes = fs::read(&args.witness)
        .with_context(|| format!("failed to read {}", args.witness.display()))?;
    let elf = fs::read(&args.elf)
        .with_context(|| format!("failed to read ELF {}", args.elf.display()))?;

    let mut stdin = SP1Stdin::new();
    stdin.write_vec(witness_bytes);

    tokio::runtime::Runtime::new()?.block_on(async {
        let client = ProverClient::builder().cpu().build().await;
        let (public_values, report) = client
            .execute(elf.into(), stdin)
            .await
            .context("SP1 execution failed")?;

        println!("execution succeeded");
        println!("total cycles:  {}", report.total_instruction_count());
        println!("public values: 0x{}", hex::encode(public_values.as_slice()));
        Ok(())
    })
}

fn sp1_prove(args: Sp1ProveArgs) -> Result<()> {
    use sp1_sdk::SP1ProofMode;
    use world_chain_proof_succinct_host_utils::{
        env_prover::EnvSuccinctProver,
        validity::{ValidityProofRequest, prove_validity},
    };

    let host = online_host_config(&args.rpc)?;

    let range_elf = fs::read(&args.range_elf)
        .with_context(|| format!("failed to read {}", args.range_elf.display()))?;
    let agg_elf = fs::read(&args.agg_elf)
        .with_context(|| format!("failed to read {}", args.agg_elf.display()))?;

    let mode = match args.mode {
        Sp1Mode::Core => SP1ProofMode::Core,
        Sp1Mode::Compressed => SP1ProofMode::Compressed,
        Sp1Mode::Plonk => SP1ProofMode::Plonk,
        Sp1Mode::Groth16 => SP1ProofMode::Groth16,
    };

    println!(
        "proving blocks {start}..={end} over {ranges} range(s) ({mode:?} aggregation, {prover:?} prover)",
        start = args.rpc.start_block + 1,
        end = args.rpc.end_block,
        ranges = args.ranges.max(1),
        mode = args.mode,
        prover = args.prover,
    );

    let prover = EnvSuccinctProver::new(args.prover, range_elf, agg_elf, mode)?;
    let artifact = prove_validity(
        &host,
        &prover,
        ValidityProofRequest {
            start_block: args.rpc.start_block,
            end_block: args.rpc.end_block,
            l1_head: args.rpc.l1_head,
            allow_unfinalized: args.rpc.allow_unfinalized,
            split_count: args.ranges.max(1),
            prover_address: args.prover_address,
        },
    )?;

    println!(
        "aggregation proof complete: block {block} pre={pre:?} post={post:?}",
        block = artifact.outputs.l2BlockNumber,
        pre = artifact.outputs.l2PreRoot,
        post = artifact.outputs.l2PostRoot,
    );

    if let Some(path) = args.output {
        write_json(&path, &artifact)?;
        println!("proof written to {}", path.display());
    }

    Ok(())
}

fn sp1_vkeys(args: Sp1VkeysArgs) -> Result<()> {
    use anyhow::anyhow;
    use sha2::{Digest, Sha256};
    use sp1_sdk::{CpuProver, HashableKey, Prover, ProvingKey, env::EnvProver};
    use world_chain_proof_core::types::u32_to_u8;

    let range_elf = fs::read(&args.range_elf)
        .with_context(|| format!("failed to read ELF {}", args.range_elf.display()))?;
    let agg_elf = fs::read(&args.agg_elf)
        .with_context(|| format!("failed to read ELF {}", args.agg_elf.display()))?;

    let range_elf_sha256 = hex::encode(Sha256::digest(&range_elf));
    let agg_elf_sha256 = hex::encode(Sha256::digest(&agg_elf));

    let (range_vkey_commitment, aggregation_vkey) =
        tokio::runtime::Runtime::new()?.block_on(async {
            let client = EnvProver::Cpu(CpuProver::new().await);
            let range_pk = client
                .setup(range_elf.into())
                .await
                .map_err(|e| anyhow!("range setup failed: {e}"))?;
            let agg_pk = client
                .setup(agg_elf.into())
                .await
                .map_err(|e| anyhow!("aggregation setup failed: {e}"))?;
            let range_vkey_commitment = B256::from(u32_to_u8(range_pk.verifying_key().hash_u32()));
            let aggregation_vkey = agg_pk.verifying_key().bytes32();
            anyhow::Ok((range_vkey_commitment, aggregation_vkey))
        })?;

    let out = serde_json::to_string_pretty(&json!({
        "range_vkey_commitment": range_vkey_commitment,
        "aggregation_vkey": aggregation_vkey,
        "elfs": {
            "world-chain-range-ethereum": {
                "path": args.range_elf,
                "sha256": range_elf_sha256,
            },
            "world-chain-aggregation": {
                "path": args.agg_elf,
                "sha256": agg_elf_sha256,
            },
        },
    }))?;

    match &args.output {
        Some(path) => {
            ensure_parent_dir(path)?;
            fs::write(path, &out).with_context(|| format!("failed to write {}", path.display()))?;
            println!("wrote vkeys to {}", path.display());
        }
        None => println!("{out}"),
    }
    Ok(())
}
