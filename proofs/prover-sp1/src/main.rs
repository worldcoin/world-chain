use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};

use alloy_primitives::{Address, B256};
use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use serde_json::json;
use world_chain_proof_succinct_host_utils::Sp1ProverKind;
use world_chain_proof_succinct_utils::RangeProofRequest;
use world_chain_prover::{
    HashRollupConfigArgs, RpcArgs, WitnessArgs, build_range_input_from_args, ensure_parent_dir,
    online_host_config, print_rollup_config_hash, write_json, write_witness,
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
    /// Build a witness and estimate SP1 range PGUs without generating a proof.
    Execute(Box<Sp1ExecuteArgs>),
    /// Generate range + aggregation proofs end-to-end from RPC.
    Prove(Box<Sp1ProveArgs>),
    /// Compute the on-chain verification keys for the range and aggregation ELFs.
    Vkeys(Sp1VkeysArgs),
}

#[derive(Debug, Args)]
struct Sp1ExecuteArgs {
    #[command(flatten)]
    rpc: RpcArgs,
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

    /// Prover backend.
    #[arg(
        long,
        env = "SP1_PROVER",
        default_value_t = Sp1ProverKind::Cpu
    )]
    prover: Sp1ProverKind,

    /// SP1 network private key. Required when --prover network.
    #[arg(long, env = "SP1_PRIVATE_KEY")]
    sp1_private_key: Option<String>,

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
    /// Output path for the vkeys JSON. Printed to stdout when unset.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    match Cli::parse().command {
        Command::HashRollupConfig(args) => print_rollup_config_hash(args).await?,
        Command::Witness(args) => write_witness(args).await?,
        Command::Execute(args) => sp1_execute(*args).await?,
        Command::Prove(args) => sp1_prove(*args).await?,
        Command::Vkeys(args) => sp1_vkeys(args).await?,
    }

    Ok(())
}

async fn sp1_execute(args: Sp1ExecuteArgs) -> Result<()> {
    use sp1_sdk::{Prover, ProverClient, SP1Stdin};

    let input = build_range_input_from_args(&args.rpc)
        .await
        .context("failed to build SP1 range witness")?;
    let request = RangeProofRequest::from_witness_data(&input.witness)
        .context("failed to serialize SP1 range witness")?;

    let mut stdin = SP1Stdin::new();
    stdin.write_vec(request.witness_rkyv);

    let client = ProverClient::builder().cpu().build().await;
    let (public_values, report) = client
        .execute(world_chain_proof_succinct_elfs::range_elf(), stdin)
        .calculate_gas(true)
        .await
        .context("SP1 execution failed")?;
    let pgus = report
        .gas()
        .context("SP1 execution report did not include a PGU estimate")?;

    println!("execution succeeded");
    println!(
        "range:          {}..={}",
        input.metadata.start_block + 1,
        input.metadata.end_block
    );
    println!("estimated PGUs: {pgus}");
    println!("total cycles:   {}", report.total_instruction_count());
    println!("total syscalls: {}", report.total_syscall_count());
    println!(
        "public values:  0x{}",
        hex::encode(public_values.as_slice())
    );
    Ok(())
}

async fn sp1_prove(args: Sp1ProveArgs) -> Result<()> {
    use sp1_sdk::SP1ProofMode;
    use world_chain_proof_succinct_host_utils::{
        cpu_prover::CpuSuccinctProver,
        mock_prover::MockSuccinctProver,
        network_prover::NetworkSuccinctProver,
        validity::{ValidityProofRequest, prove_validity},
    };

    let host = online_host_config(&args.rpc)?;

    let mode = match args.mode {
        Sp1Mode::Core => SP1ProofMode::Core,
        Sp1Mode::Compressed => SP1ProofMode::Compressed,
        Sp1Mode::Plonk => SP1ProofMode::Plonk,
        Sp1Mode::Groth16 => SP1ProofMode::Groth16,
    };

    println!(
        "proving blocks {start}..={end} over {ranges} range(s) ({mode:?} aggregation, {prover} prover)",
        start = args.rpc.start_block + 1,
        end = args.rpc.end_block,
        ranges = args.ranges.max(1),
        mode = args.mode,
        prover = args.prover,
    );

    let proof_request = ValidityProofRequest {
        start_block: args.rpc.start_block,
        end_block: args.rpc.end_block,
        l1_head: args.rpc.l1_head,
        allow_unfinalized: args.rpc.allow_unfinalized,
        split_count: args.ranges.max(1),
        prover_address: args.prover_address,
    };

    let artifact = match args.prover {
        Sp1ProverKind::Cpu => {
            let prover = CpuSuccinctProver::new(mode).await?;
            prove_validity(&host, &prover, proof_request.clone()).await?
        }
        Sp1ProverKind::Mock => {
            let prover = MockSuccinctProver::new(mode).await?;
            prove_validity(&host, &prover, proof_request).await?
        }
        Sp1ProverKind::Network => {
            let private_key = args
                .sp1_private_key
                .clone()
                .context("SP1_PRIVATE_KEY is required when --prover network")?;
            let prover = NetworkSuccinctProver::new(mode, &private_key).await?;
            prove_validity(&host, &prover, proof_request).await?
        }
    };

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

async fn sp1_vkeys(args: Sp1VkeysArgs) -> Result<()> {
    use anyhow::anyhow;
    use sp1_sdk::{CpuProver, HashableKey, Prover, ProvingKey, env::EnvProver};
    use world_chain_proof_core::types::u32_to_u8;
    let range_elf_bytes = world_chain_proof_succinct_elfs::range_elf();
    let agg_elf_bytes = world_chain_proof_succinct_elfs::aggregation_elf();

    let range_elf_sha256 = hex::encode(Sha256::digest(&*range_elf_bytes));
    let agg_elf_sha256 = hex::encode(Sha256::digest(&*agg_elf_bytes));

    let (range_vkey_commitment, aggregation_vkey) = {
        let client = EnvProver::Cpu(CpuProver::new().await);
        let range_pk = client
            .setup(range_elf_bytes)
            .await
            .map_err(|e| anyhow!("range setup failed: {e}"))?;
        let agg_pk = client
            .setup(agg_elf_bytes)
            .await
            .map_err(|e| anyhow!("aggregation setup failed: {e}"))?;
        let range_vkey_commitment = B256::from(u32_to_u8(range_pk.verifying_key().hash_u32()));
        let aggregation_vkey = agg_pk.verifying_key().bytes32();
        anyhow::Ok((range_vkey_commitment, aggregation_vkey))
    }?;

    let out = serde_json::to_string_pretty(&json!({
        "range_vkey_commitment": range_vkey_commitment,
        "aggregation_vkey": aggregation_vkey,
        "elfs": {
            "world-chain-range-ethereum": {
                "sha256": range_elf_sha256,
            },
            "world-chain-aggregation": {
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

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::*;

    fn execute_args() -> Vec<&'static str> {
        vec![
            "world-chain-prover-sp1",
            "execute",
            "--start-block",
            "100",
            "--end-block",
            "110",
            "--l2-rpc",
            "http://localhost:9545",
            "--l1-rpc",
            "http://localhost:8545",
            "--l1-beacon-rpc",
            "http://localhost:5052",
            "--rollup-config-hash",
            "0x0000000000000000000000000000000000000000000000000000000000000000",
        ]
    }

    fn rpc_args() -> RpcArgs {
        RpcArgs {
            start_block: 100,
            end_block: 110,
            l2_rpc: "http://localhost:9545".to_string(),
            l1_rpc: "http://localhost:8545".to_string(),
            l1_beacon_rpc: "http://localhost:5052".to_string(),
            rollup_config: None,
            rollup_config_hash: Some(B256::ZERO),
            l1_head: None,
            allow_unfinalized: false,
            witness_timeout_seconds: 900,
            network: world_chain_prover::Network::WorldChain,
        }
    }

    #[test]
    fn execute_parses_rpc_backed_range() {
        let cli =
            Cli::try_parse_from(execute_args()).expect("RPC-backed execute arguments should parse");

        let Command::Execute(args) = cli.command else {
            panic!("expected execute command");
        };
        assert_eq!(args.rpc.start_block, 100);
        assert_eq!(args.rpc.end_block, 110);
        assert_eq!(args.rpc.network.chain_id(), 480);
    }

    #[test]
    fn execute_requires_an_explicit_block_range() {
        let error = Cli::try_parse_from([
            "world-chain-prover-sp1",
            "execute",
            "--l2-rpc",
            "http://localhost:9545",
            "--l1-rpc",
            "http://localhost:8545",
            "--l1-beacon-rpc",
            "http://localhost:5052",
        ])
        .expect_err("execute should require start and end blocks");

        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn execute_rejects_legacy_file_arguments() {
        for (flag, value) in [("--witness", "witness.bin"), ("--elf", "range-elf")] {
            let mut args = execute_args();
            args.extend([flag, value]);
            let error = Cli::try_parse_from(args)
                .expect_err("execute should no longer accept witness or ELF paths");

            assert_eq!(error.kind(), ErrorKind::UnknownArgument, "flag: {flag}");
        }
    }

    #[tokio::test]
    async fn execute_requires_rollup_config_identity() {
        let mut rpc = rpc_args();
        rpc.rollup_config_hash = None;

        let error = sp1_execute(Sp1ExecuteArgs { rpc })
            .await
            .expect_err("execute should require a rollup config or hash");

        assert!(
            format!("{error:#}").contains("provide --rollup-config"),
            "unexpected error: {error:#}"
        );
    }

    #[tokio::test]
    async fn execute_rejects_an_invalid_block_range() {
        let mut rpc = rpc_args();
        rpc.start_block = 110;
        rpc.end_block = 100;

        let error = sp1_execute(Sp1ExecuteArgs { rpc })
            .await
            .expect_err("execute should reject an inverted block range");

        assert!(
            format!("{error:#}").contains("end_block (100) must be greater than start_block (110)"),
            "unexpected error: {error:#}"
        );
    }
}
