use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[cfg(feature = "sp1")]
use alloy_primitives::Address;
use alloy_primitives::B256;
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use reqwest::blocking::Client;
use serde::Serialize;
use serde_json::{Value, json};
use world_chain_chainspec::WorldChainSpec;
use world_chain_proof_core::{range::WorldRangeHardforkConfig, witness::WorldRangeWitnessData};
use world_chain_proof_kona_host_utils::online::{
    OnlineHostConfig, RangeProofInput, RangeWitnessRequest, build_range_input, rpc,
};
use world_chain_proof_protocol::WorldHardforkConfig as ProtocolHardforkConfig;

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Network {
    #[value(name = "worldchain")]
    WorldChain,
    #[value(name = "worldchain-sepolia")]
    WorldChainSepolia,
}

impl Network {
    fn chain_id(self) -> u64 {
        match self {
            Self::WorldChain => 480,
            Self::WorldChainSepolia => 4801,
        }
    }

    fn chain_spec(self) -> Arc<WorldChainSpec> {
        match self {
            Self::WorldChain => WorldChainSpec::mainnet(),
            Self::WorldChainSepolia => WorldChainSpec::sepolia(),
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "world-chain-proof-witness-gen",
    about = "World Chain witness generator and Nitro enclave prover"
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
    /// AWS Nitro TEE proving.
    #[cfg(all(feature = "nitro", target_os = "linux"))]
    Nitro {
        #[command(subcommand)]
        command: NitroCommand,
    },
    /// SP1 zkVM proving.
    #[cfg(feature = "sp1")]
    Sp1 {
        #[command(subcommand)]
        command: Sp1Command,
    },
}

#[cfg(all(feature = "nitro", target_os = "linux"))]
#[derive(Debug, Subcommand)]
enum NitroCommand {
    /// Generate witness and send to a running Nitro enclave for attested proving.
    Prove(NitroArgs),
}

#[cfg(feature = "sp1")]
#[derive(Debug, Subcommand)]
enum Sp1Command {
    /// Execute the SP1 range program against a witness file (no ZK proof, fast).
    Execute(Sp1ExecuteArgs),
    /// Generate range + aggregation proofs end-to-end from RPC.
    Prove(Box<Sp1ProveArgs>),
    /// Compute the on-chain verification keys for the range and aggregation ELFs.
    Vkeys(Sp1VkeysArgs),
}

#[derive(Debug, Args)]
struct HashRollupConfigArgs {
    /// Rollup config JSON file. Mutually exclusive with --l2-rpc.
    #[arg(long, env = "ROLLUP_CONFIG", conflicts_with = "l2_rpc")]
    rollup_config: Option<PathBuf>,

    /// L2 RPC URL to fetch the rollup config from. Mutually exclusive with --rollup-config.
    #[arg(long, env = "L2_RPC_URL", conflicts_with = "rollup_config")]
    l2_rpc: Option<String>,
}

#[derive(Debug, Clone, Args)]
struct RpcArgs {
    /// L2 block number to start from (exclusive lower bound; proved range is start+1..=end).
    #[arg(long)]
    start_block: u64,

    /// L2 block number to prove up to (inclusive).
    #[arg(long)]
    end_block: u64,

    /// World Chain L2 execution RPC URL.
    #[arg(long, env = "L2_RPC_URL")]
    l2_rpc: String,

    /// Ethereum L1 execution RPC URL.
    #[arg(long, env = "L1_RPC_URL")]
    l1_rpc: String,

    /// Ethereum L1 beacon API URL.
    #[arg(long, env = "L1_BEACON_RPC_URL")]
    l1_beacon_rpc: String,

    /// Rollup config JSON file. If omitted, uses the built-in World Chain mainnet config.
    #[arg(long, env = "ROLLUP_CONFIG")]
    rollup_config: Option<PathBuf>,

    /// Rollup config hash override (required when --rollup-config is not supplied).
    #[arg(long, env = "ROLLUP_CONFIG_HASH")]
    rollup_config_hash: Option<B256>,

    /// L1 head hash override. Defaults to a finalized L1 block after the L2 range.
    #[arg(long, env = "L1_HEAD")]
    l1_head: Option<B256>,

    /// Allow proving blocks newer than the finalized L2 head.
    #[arg(long)]
    allow_unfinalized: bool,

    /// Maximum seconds to spend generating the Kona witness.
    #[arg(long, default_value_t = 900)]
    witness_timeout_seconds: u64,

    /// World Chain network to prove.
    #[arg(long, env = "NETWORK", default_value = "worldchain")]
    network: Network,
}

#[derive(Debug, Args)]
struct WitnessArgs {
    #[command(flatten)]
    rpc: RpcArgs,

    /// Output path for the rkyv-serialized witness bytes.
    #[arg(long)]
    output: PathBuf,
}

#[cfg(all(feature = "nitro", target_os = "linux"))]
#[derive(Debug, Args)]
struct NitroArgs {
    #[command(flatten)]
    rpc: RpcArgs,

    /// vsock CID of the running Nitro enclave.
    #[arg(long, env = "ENCLAVE_CID", default_value_t = 16)]
    cid: u32,

    /// PCR0 hex (48 bytes). Leave unset to skip PCR verification (testing only).
    #[arg(long, env = "PCR0")]
    pcr0: Option<String>,

    /// PCR1 hex (48 bytes). Leave unset to skip PCR verification (testing only).
    #[arg(long, env = "PCR1")]
    pcr1: Option<String>,

    /// PCR2 hex (48 bytes). Leave unset to skip PCR verification (testing only).
    #[arg(long, env = "PCR2")]
    pcr2: Option<String>,

    /// Output path for the JSON artifact (boot info + attestation doc).
    #[arg(long)]
    output: Option<PathBuf>,
}

#[cfg(feature = "sp1")]
#[derive(Debug, Args)]
struct Sp1ExecuteArgs {
    /// rkyv-serialized witness file produced by the `witness` subcommand.
    #[arg(long, env = "WITNESS_PATH")]
    witness: PathBuf,
}

#[cfg(feature = "sp1")]
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

#[cfg(feature = "sp1")]
#[derive(Debug, Args)]
struct Sp1ProveArgs {
    #[command(flatten)]
    rpc: RpcArgs,

    /// Number of equal-length sub-ranges to split the block range into.
    #[arg(long, default_value_t = 1)]
    ranges: u64,

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

#[cfg(feature = "sp1")]
#[derive(Debug, Args)]
struct Sp1VkeysArgs {
    /// Output path for the vkeys JSON. Printed to stdout when unset.
    #[arg(long)]
    output: Option<PathBuf>,
}

fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    match Cli::parse().command {
        Command::HashRollupConfig(args) => {
            let hash = match (args.rollup_config, args.l2_rpc) {
                (Some(path), _) => proof_config_from_file(&path)?.1,
                (None, Some(url)) => {
                    let client = Client::new();
                    let value: Value = rpc(&client, &url, "optimism_rollupConfig", json!([]))?
                        .context("optimism_rollupConfig returned null")?;
                    world_chain_proof_protocol::hash_rollup_config(&value)?
                }
                (None, None) => bail!("provide --rollup-config or --l2-rpc"),
            };
            println!("{hash:?}");
        }
        Command::Witness(args) => {
            let input = build_range_input_from_args(&args.rpc)?;
            let bytes = witness_bytes(&input.witness)?;
            write_bytes(&args.output, &bytes)?;
            let metadata_path = sibling_path(&args.output, "metadata.json");
            write_json(&metadata_path, &json!({ "metadata": input.metadata }))?;
            println!("witness bytes: {}", args.output.display());
            println!("metadata:      {}", metadata_path.display());
        }
        #[cfg(all(feature = "nitro", target_os = "linux"))]
        Command::Nitro { command } => match command {
            NitroCommand::Prove(args) => nitro_prove(args)?,
        },
        #[cfg(feature = "sp1")]
        Command::Sp1 { command } => match command {
            Sp1Command::Execute(args) => sp1_execute(args)?,
            Sp1Command::Prove(args) => sp1_prove(*args)?,
            Sp1Command::Vkeys(args) => sp1_vkeys(args)?,
        },
    }

    Ok(())
}

/// Resolves the online host config (RPC endpoints + proof config) from CLI args.
fn online_host_config(args: &RpcArgs) -> Result<OnlineHostConfig> {
    let (schedule, rollup_config_hash) = proof_config(
        args.network,
        args.rollup_config.as_deref(),
        args.rollup_config_hash,
    )?;

    Ok(OnlineHostConfig {
        l1_rpc: args.l1_rpc.clone(),
        l1_beacon_rpc: args.l1_beacon_rpc.clone(),
        l2_rpc: args.l2_rpc.clone(),
        schedule,
        rollup_config_hash,
        l2_chain_id: args
            .rollup_config
            .is_none()
            .then_some(args.network.chain_id()),
        rollup_config_path: args.rollup_config.clone(),
        witness_timeout: Duration::from_secs(args.witness_timeout_seconds),
    })
}

fn build_range_input_from_args(args: &RpcArgs) -> Result<RangeProofInput> {
    let config = online_host_config(args)?;
    build_range_input(
        &config,
        RangeWitnessRequest {
            start_block: args.start_block,
            end_block: args.end_block,
            l1_head: args.l1_head,
            allow_unfinalized: args.allow_unfinalized,
        },
    )
}

#[cfg(all(feature = "nitro", target_os = "linux"))]
fn nitro_prove(args: NitroArgs) -> Result<()> {
    use anyhow::anyhow;
    use world_chain_proof_nitro::{
        ExpectedPcrs, NitroRangeProofRequest,
        attestation::parse_and_check_pcrs,
        host::{EnclaveEndpoint, NitroProver},
        protocol::range_user_data,
    };

    let input = build_range_input_from_args(&args.rpc)?;

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

    let rt = tokio::runtime::Runtime::new()?;
    let prover = NitroProver::with_runtime(
        EnclaveEndpoint::new(args.cid),
        expected_pcrs,
        rt.handle().clone(),
    );

    println!(
        "sending range {start}..={end} to enclave (cid {cid})",
        start = args.rpc.start_block + 1,
        end = args.rpc.end_block,
        cid = args.cid,
    );

    let artifact = rt
        .block_on(prover.prove_range_async(request))
        .map_err(|e| anyhow!("enclave proving failed: {e}"))?;

    println!(
        "enclave returned: l2_pre={pre:?} l2_post={post:?} block={block}",
        pre = artifact.boot_info.l2PreRoot,
        post = artifact.boot_info.l2PostRoot,
        block = artifact.boot_info.l2BlockNumber,
    );

    let expected_user_data = range_user_data(&artifact.boot_info);
    parse_and_check_pcrs(
        &artifact.attestation_doc,
        &expected_pcrs,
        &expected_user_data,
    )
    .map_err(|e| anyhow!("attestation verification failed: {e}"))?;

    println!("attestation verified OK");
    println!("{}", serde_json::to_string_pretty(&artifact.boot_info)?);

    if let Some(output) = args.output {
        write_json(
            &output,
            &json!({
                "bootInfo": artifact.boot_info,
                "attestationDoc": format!("0x{}", hex::encode(&artifact.attestation_doc)),
            }),
        )?;
        println!("artifact written to {}", output.display());
    }

    Ok(())
}

#[cfg(all(feature = "nitro", target_os = "linux"))]
fn hex_to_pcr(hex: &str) -> Result<[u8; 48]> {
    let bytes = hex::decode(hex).context("invalid PCR hex")?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("PCR must be 48 bytes"))
}

fn proof_config(
    network: Network,
    rollup_config_path: Option<&Path>,
    rollup_config_hash: Option<B256>,
) -> Result<(WorldRangeHardforkConfig, B256)> {
    if let Some(path) = rollup_config_path {
        return proof_config_from_file(path);
    }

    let hash = rollup_config_hash
        .context("provide --rollup-config or ROLLUP_CONFIG, or supply --rollup-config-hash")?;
    let spec = network.chain_spec();
    let protocol_config = ProtocolHardforkConfig::from_chain_spec(spec.as_ref());
    Ok((range_hardfork_config(&protocol_config), hash))
}

fn proof_config_from_file(path: &Path) -> Result<(WorldRangeHardforkConfig, B256)> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let protocol_config = ProtocolHardforkConfig::from_rollup_config_value(&value)?;
    let hash = world_chain_proof_protocol::hash_rollup_config(&value)?;
    Ok((range_hardfork_config(&protocol_config), hash))
}

fn range_hardfork_config(config: &ProtocolHardforkConfig) -> WorldRangeHardforkConfig {
    WorldRangeHardforkConfig {
        bedrock_block: config.bedrock_block,
        regolith_time: config.regolith_time,
        canyon_time: config.canyon_time,
        ecotone_time: config.ecotone_time,
        fjord_time: config.fjord_time,
        granite_time: config.granite_time,
        holocene_time: config.holocene_time,
        isthmus_time: config.isthmus_time,
        jovian_time: config.jovian_time,
        tropo_time: config.tropo_time,
        strato_time: config.strato_time,
    }
}

fn witness_bytes(witness: &WorldRangeWitnessData) -> Result<Vec<u8>> {
    Ok(rkyv::to_bytes::<rkyv::rancor::Error>(witness)?.to_vec())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    ensure_parent_dir(path)?;
    fs::write(path, serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("failed to write {}", path.display()))
}

fn write_bytes(path: &Path, value: &[u8]) -> Result<()> {
    ensure_parent_dir(path)?;
    fs::write(path, value).with_context(|| format!("failed to write {}", path.display()))
}

fn sibling_path(base: &Path, suffix: &str) -> PathBuf {
    let stem = base
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("witness");
    base.with_file_name(format!("{stem}.{suffix}"))
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    Ok(())
}

#[cfg(feature = "sp1")]
fn sp1_execute(args: Sp1ExecuteArgs) -> Result<()> {
    use sp1_sdk::{Prover, ProverClient, SP1Stdin};
    use world_chain_proof_succinct_host_utils::env_prover::range_elf;

    let witness_bytes = fs::read(&args.witness)
        .with_context(|| format!("failed to read {}", args.witness.display()))?;

    let mut stdin = SP1Stdin::new();
    stdin.write_vec(witness_bytes);

    tokio::runtime::Runtime::new()?.block_on(async {
        let client = ProverClient::builder().cpu().build().await;
        let (public_values, report) = client
            .execute(range_elf(), stdin)
            .await
            .context("SP1 execution failed")?;

        println!("execution succeeded");
        println!("total cycles:  {}", report.total_instruction_count());
        println!("public values: 0x{}", hex::encode(public_values.as_slice()));
        Ok(())
    })
}

#[cfg(feature = "sp1")]
fn sp1_prove(args: Sp1ProveArgs) -> Result<()> {
    use sp1_sdk::SP1ProofMode;
    use world_chain_proof_succinct_host_utils::{
        env_prover::EnvSuccinctProver,
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
        "proving blocks {start}..={end} over {ranges} range(s) ({mode:?} aggregation, {prover:?} prover)",
        start = args.rpc.start_block + 1,
        end = args.rpc.end_block,
        ranges = args.ranges.max(1),
        mode = args.mode,
        prover = args.prover,
    );

    let prover = EnvSuccinctProver::new(args.prover, mode)?;
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

#[cfg(feature = "sp1")]
fn sp1_vkeys(args: Sp1VkeysArgs) -> Result<()> {
    use anyhow::anyhow;
    use sha2::{Digest, Sha256};
    use sp1_sdk::{CpuProver, HashableKey, Prover, ProvingKey, env::EnvProver};
    use world_chain_proof_core::types::u32_to_u8;
    use world_chain_proof_succinct_host_utils::env_prover::{aggregation_elf, range_elf};

    let range_elf = range_elf();
    let agg_elf = aggregation_elf();

    let range_elf_sha256 = hex::encode(Sha256::digest(&*range_elf));
    let agg_elf_sha256 = hex::encode(Sha256::digest(&*agg_elf));

    let (range_vkey_commitment, aggregation_vkey) =
        tokio::runtime::Runtime::new()?.block_on(async {
            let client = EnvProver::Cpu(CpuProver::new().await);
            let range_pk = client
                .setup(range_elf)
                .await
                .map_err(|e| anyhow!("range setup failed: {e}"))?;
            let agg_pk = client
                .setup(agg_elf)
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
            "world-chain-proof-succinct-range-ethereum": {
                "sha256": range_elf_sha256,
            },
            "world-chain-proof-succinct-aggregation": {
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
