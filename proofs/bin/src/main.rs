use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy_primitives::{Address, B256, address, keccak256};
use clap::{Args, Parser, Subcommand};
use color_eyre::eyre::{self, Context, ContextCompat, eyre};
use kona_host::{DataFormat, single::SingleChainHost};
use kona_preimage::{BidirectionalChannel, HintWriter, NativeChannel, OracleReader};
use kona_proof::{CachingOracle, l1::OracleBlobProvider};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use world_chain_chainspec::WorldChainSpec;
use world_chain_proof_core::{
    range::{WorldRangeHardforkConfig, WorldRangeSpecId},
    witness::{BlobData, WorldRangeWitnessData, preimage_store::PreimageStore},
};
use world_chain_proof_protocol::WorldHardforkConfig as ProtocolHardforkConfig;
use world_chain_proof_succinct_client_utils::{
    OutputRootWitness,
    witness::executor::{WitnessExecutor, get_inputs_for_pipeline},
};
use world_chain_proof_succinct_ethereum_client_utils::executor::ETHDAWitnessExecutor;
use world_chain_proof_succinct_host_utils::witness_generation::{
    OnlineBlobStore, PreimageWitnessCollector,
};

const L1_BLOCK_PREDEPLOY: Address = address!("0x4200000000000000000000000000000000000015");
const L2_TO_L1_MESSAGE_PASSER: Address = address!("0x4200000000000000000000000000000000000016");

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

type DefaultOracleBase = CachingOracle<OracleReader<NativeChannel>, HintWriter<NativeChannel>>;
type WorldPreimageCollector = PreimageWitnessCollector<DefaultOracleBase>;
type WorldOnlineBlobStore = OnlineBlobStore<OracleBlobProvider<DefaultOracleBase>>;
type WorldEthWitnessExecutor = ETHDAWitnessExecutor<WorldPreimageCollector, WorldOnlineBlobStore>;

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

    /// Path to the SP1 range ELF binary.
    #[arg(long, env = "RANGE_ELF_PATH")]
    elf: PathBuf,
}

#[cfg(feature = "sp1")]
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Sp1Prover {
    /// Local CPU prover (requires 32–128 GB RAM).
    #[value(name = "cpu")]
    Cpu,
    /// Succinct proving network (requires SP1_PRIVATE_KEY env var).
    #[value(name = "network")]
    Network,
    /// Mock prover — no real ZK, for integration testing only.
    #[value(name = "mock")]
    Mock,
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

    /// Path to the SP1 range ELF binary.
    #[arg(long, env = "RANGE_ELF_PATH")]
    range_elf: PathBuf,

    /// Path to the SP1 aggregation ELF binary.
    #[arg(long, env = "AGG_ELF_PATH")]
    agg_elf: PathBuf,

    /// Prover backend. Overrides SP1_PROVER env var.
    #[arg(long, env = "SP1_PROVER", default_value = "cpu")]
    prover: Sp1Prover,

    /// Aggregation proof mode.
    #[arg(long, default_value = "groth16")]
    mode: Sp1Mode,

    /// Prover address for on-chain attribution (defaults to zero address).
    #[arg(long, default_value = "0x0000000000000000000000000000000000000000")]
    prover_address: Address,

    /// Output path for the aggregation proof JSON.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[cfg(feature = "sp1")]
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

struct RangeProofInput {
    metadata: RangeMetadata,
    witness: WorldRangeWitnessData,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RangeMetadata {
    start_block: u64,
    end_block: u64,
    finalized_l2_head: Option<u64>,
    l1_head: B256,
    l2_pre_root: B256,
    l2_post_root: B256,
    rollup_config_hash: B256,
    active_fork: String,
    world_spec_id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RpcBlock {
    number: HexU64,
    hash: B256,
    state_root: B256,
    timestamp: HexU64,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(try_from = "String")]
struct HexU64(u64);

impl TryFrom<String> for HexU64 {
    type Error = eyre::Report;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let value = value
            .strip_prefix("0x")
            .context("hex quantity must start with 0x")?;
        Ok(Self(u64::from_str_radix(value, 16)?))
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountProof {
    storage_hash: B256,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OptimismOutputAtBlock {
    output_root: B256,
    withdrawal_storage_root: B256,
    state_root: B256,
}

#[derive(Clone, Debug, Deserialize)]
struct RpcResponse<T> {
    result: Option<T>,
    error: Option<RpcError>,
}

#[derive(Clone, Debug, Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

fn main() -> eyre::Result<()> {
    color_eyre::install()?;
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
                (None, None) => return Err(eyre!("provide --rollup-config or --l2-rpc")),
            };
            println!("{hash:?}");
        }
        Command::Witness(args) => {
            let input = build_range_input(&args.rpc)?;
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

#[cfg(all(feature = "nitro", target_os = "linux"))]
fn nitro_prove(args: NitroArgs) -> eyre::Result<()> {
    use world_chain_proof_nitro::{
        ExpectedPcrs, NitroRangeProofRequest,
        attestation::parse_and_check_pcrs,
        host::{EnclaveEndpoint, NitroProver},
        protocol::range_user_data,
    };

    let input = build_range_input(&args.rpc)?;

    let expected_pcrs = match (args.pcr0, args.pcr1, args.pcr2) {
        (Some(p0), Some(p1), Some(p2)) => ExpectedPcrs {
            pcr0: hex_to_pcr(&p0)?,
            pcr1: hex_to_pcr(&p1)?,
            pcr2: hex_to_pcr(&p2)?,
        },
        (None, None, None) => {
            return Err(eyre!(
                "--pcr0/--pcr1/--pcr2 are required: real PCR measurements must be supplied to verify the enclave image"
            ));
        }
        _ => return Err(eyre!("provide all three of --pcr0, --pcr1, --pcr2 or none")),
    };

    let request = NitroRangeProofRequest::from_witness_data(&input.witness, None)
        .map_err(|e| eyre!("failed to serialize witness: {e}"))?;

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
        .map_err(|e| eyre!("enclave proving failed: {e}"))?;

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
    .map_err(|e| eyre!("attestation verification failed: {e}"))?;

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
fn hex_to_pcr(hex: &str) -> eyre::Result<[u8; 48]> {
    let bytes = hex::decode(hex).context("invalid PCR hex")?;
    bytes.try_into().map_err(|_| eyre!("PCR must be 48 bytes"))
}

fn build_range_input(args: &RpcArgs) -> eyre::Result<RangeProofInput> {
    if args.end_block <= args.start_block {
        return Err(eyre!(
            "end_block ({}) must be greater than start_block ({})",
            args.end_block,
            args.start_block,
        ));
    }

    let client = Client::new();
    let finalized_l2_head = finalized_l2_head(&client, &args.l2_rpc)?;
    if !args.allow_unfinalized && finalized_l2_head.is_some_and(|head| args.end_block > head) {
        return Err(eyre!(
            "end_block {} is newer than finalized L2 head {}; pass --allow-unfinalized to override",
            args.end_block,
            finalized_l2_head.unwrap(),
        ));
    }

    let (schedule, rollup_config_hash) = proof_config(
        args.network,
        args.rollup_config.as_deref(),
        args.rollup_config_hash,
    )?;

    let pre_block = get_block(&client, &args.l2_rpc, BlockTag::Number(args.start_block))?;
    let post_block = get_block(&client, &args.l2_rpc, BlockTag::Number(args.end_block))?;
    let pre_state = output_root_witness(&client, &args.l2_rpc, args.start_block, &pre_block)?;
    let post_state = output_root_witness(&client, &args.l2_rpc, args.end_block, &post_block)?;
    let pre_root = pre_state.output_root();
    let post_root = post_state.output_root();

    let l1_head = match args.l1_head {
        Some(hash) => hash,
        None => resolve_l1_head(&client, &args.l2_rpc, &args.l1_rpc, args.end_block)?,
    };

    let active_fork = schedule.active_fork_at(args.end_block, post_block.timestamp.0);
    let world_spec_id = WorldRangeSpecId::from_hardfork(active_fork);

    let host = SingleChainHost {
        l1_head,
        agreed_l2_head_hash: pre_block.hash,
        agreed_l2_output_root: pre_root,
        claimed_l2_output_root: post_root,
        claimed_l2_block_number: args.end_block,
        l2_node_address: Some(trim_rpc_url(&args.l2_rpc)),
        l1_node_address: Some(trim_rpc_url(&args.l1_rpc)),
        l1_beacon_address: Some(trim_rpc_url(&args.l1_beacon_rpc)),
        data_dir: None,
        data_format: DataFormat::default(),
        native: false,
        server: true,
        l2_chain_id: args
            .rollup_config
            .is_none()
            .then_some(args.network.chain_id()),
        rollup_config_path: args.rollup_config.clone(),
        l1_config_path: None,
        enable_experimental_witness_endpoint: false,
    };

    let witness = tokio::runtime::Runtime::new()?.block_on(collect_world_range_witness(
        host,
        schedule.clone(),
        Duration::from_secs(args.witness_timeout_seconds),
    ))?;

    Ok(RangeProofInput {
        metadata: RangeMetadata {
            start_block: args.start_block,
            end_block: args.end_block,
            finalized_l2_head,
            l1_head,
            l2_pre_root: pre_root,
            l2_post_root: post_root,
            rollup_config_hash,
            active_fork: format!("{active_fork:?}"),
            world_spec_id: <&'static str>::from(world_spec_id).to_string(),
        },
        witness,
    })
}

async fn collect_world_range_witness(
    host: SingleChainHost,
    schedule: WorldRangeHardforkConfig,
    timeout: Duration,
) -> eyre::Result<WorldRangeWitnessData> {
    let preimage = BidirectionalChannel::new()?;
    let hint = BidirectionalChannel::new()?;
    let mut server_task = host.start_server(hint.host, preimage.host).await?;

    let witness = collect_witness_from_channels(preimage.client, hint.client, schedule);
    tokio::pin!(witness);

    let result = match tokio::time::timeout(timeout, async {
        tokio::select! {
            result = &mut witness => result,
            server_result = &mut server_task => match server_result {
                Ok(Ok(())) => Err(eyre!("Kona preimage server exited before witness generation completed")),
                Ok(Err(err)) => Err(eyre!("Kona preimage server failed: {err}")),
                Err(err) => Err(eyre!("Kona preimage server task panicked: {err}")),
            },
        }
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(eyre!(
            "Kona witness generation timed out after {} seconds",
            timeout.as_secs()
        )),
    };

    if !server_task.is_finished() {
        server_task.abort();
    }
    result
}

async fn collect_witness_from_channels(
    preimage_chan: NativeChannel,
    hint_chan: NativeChannel,
    schedule: WorldRangeHardforkConfig,
) -> eyre::Result<WorldRangeWitnessData> {
    let preimage_witness_store = Arc::new(Mutex::new(PreimageStore::default()));
    let blob_data = Arc::new(Mutex::new(BlobData::default()));

    let preimage_oracle = Arc::new(CachingOracle::new(
        2048,
        OracleReader::new(preimage_chan),
        HintWriter::new(hint_chan),
    ));
    let blob_provider = OracleBlobProvider::new(preimage_oracle.clone());
    let oracle = Arc::new(PreimageWitnessCollector {
        preimage_oracle,
        preimage_witness_store: preimage_witness_store.clone(),
    });
    let beacon = OnlineBlobStore {
        provider: blob_provider,
        store: blob_data.clone(),
    };

    let executor = WorldEthWitnessExecutor::new();
    let (boot_info, input) = get_inputs_for_pipeline(oracle.clone())
        .await
        .map_err(|err| eyre!("failed to load Kona pipeline inputs: {err:?}"))?;

    if let Some((cursor, l1_provider, l2_provider)) = input {
        let rollup_config = Arc::new(boot_info.rollup_config.clone());
        let l1_config = Arc::new(boot_info.l1_config.clone());
        let pipeline = executor
            .create_pipeline(
                rollup_config,
                l1_config,
                cursor.clone(),
                oracle,
                beacon,
                l1_provider,
                l2_provider.clone(),
            )
            .await
            .map_err(|err| eyre!("failed to create Kona derivation pipeline: {err:?}"))?;
        executor
            .run_with_world_schedule(
                boot_info,
                pipeline,
                cursor,
                l2_provider,
                Some(schedule.clone()),
            )
            .await
            .map_err(|err| eyre!("failed to run Kona derivation/execution: {err:?}"))?;
    }

    Ok(WorldRangeWitnessData::from_parts_with_world_config(
        preimage_witness_store.lock().unwrap().clone(),
        blob_data.lock().unwrap().clone(),
        schedule,
    ))
}

fn proof_config(
    network: Network,
    rollup_config_path: Option<&Path>,
    rollup_config_hash: Option<B256>,
) -> eyre::Result<(WorldRangeHardforkConfig, B256)> {
    if let Some(path) = rollup_config_path {
        return proof_config_from_file(path);
    }

    let hash = rollup_config_hash
        .context("provide --rollup-config or ROLLUP_CONFIG, or supply --rollup-config-hash")?;
    let spec = network.chain_spec();
    let protocol_config = ProtocolHardforkConfig::from_chain_spec(spec.as_ref());
    Ok((range_hardfork_config(&protocol_config), hash))
}

fn proof_config_from_file(path: &Path) -> eyre::Result<(WorldRangeHardforkConfig, B256)> {
    let bytes = fs::read(path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .wrap_err_with(|| format!("failed to parse {}", path.display()))?;
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

fn finalized_l2_head(client: &Client, rpc_url: &str) -> eyre::Result<Option<u64>> {
    Ok(rpc::<RpcBlock>(
        client,
        rpc_url,
        "eth_getBlockByNumber",
        json!(["finalized", false]),
    )?
    .map(|b| b.number.0))
}

fn get_block(client: &Client, rpc_url: &str, tag: BlockTag) -> eyre::Result<RpcBlock> {
    rpc(
        client,
        rpc_url,
        "eth_getBlockByNumber",
        json!([tag.to_rpc_value(), false]),
    )?
    .with_context(|| format!("eth_getBlockByNumber returned null for {}", tag.display()))
}

fn output_root_witness(
    client: &Client,
    rpc_url: &str,
    block_number: u64,
    block: &RpcBlock,
) -> eyre::Result<OutputRootWitness> {
    let proof = match rpc::<AccountProof>(
        client,
        rpc_url,
        "eth_getProof",
        json!([
            format!("{L2_TO_L1_MESSAGE_PASSER:#x}"),
            Vec::<String>::new(),
            BlockTag::Number(block_number).to_rpc_value(),
        ]),
    ) {
        Ok(Some(proof)) => proof,
        Ok(None) => return Err(eyre!("eth_getProof returned null")),
        Err(proof_err) => {
            return output_root_witness_from_op_node(client, rpc_url, block_number, block)
                .wrap_err_with(|| format!("eth_getProof failed first: {proof_err}"));
        }
    };

    Ok(OutputRootWitness {
        state_root: block.state_root,
        message_passer_storage_root: proof.storage_hash,
        block_hash: block.hash,
    })
}

fn output_root_witness_from_op_node(
    client: &Client,
    rpc_url: &str,
    block_number: u64,
    block: &RpcBlock,
) -> eyre::Result<OutputRootWitness> {
    let output: OptimismOutputAtBlock = rpc(
        client,
        rpc_url,
        "optimism_outputAtBlock",
        json!([BlockTag::Number(block_number).to_rpc_value()]),
    )?
    .context("optimism_outputAtBlock returned null")?;

    let witness = OutputRootWitness {
        state_root: output.state_root,
        message_passer_storage_root: output.withdrawal_storage_root,
        block_hash: block.hash,
    };
    let computed = witness.output_root();
    if computed != output.output_root {
        return Err(eyre!(
            "output root mismatch for block {block_number}: computed {computed}, RPC returned {}",
            output.output_root
        ));
    }
    Ok(witness)
}

fn resolve_l1_head(
    client: &Client,
    l2_rpc: &str,
    l1_rpc: &str,
    end_block: u64,
) -> eyre::Result<B256> {
    let l1_origin_number = l1_origin_number(client, l2_rpc, end_block)?;
    let finalized_l1 = get_block(client, l1_rpc, BlockTag::Finalized)?;
    let target = l1_origin_number
        .saturating_add(20)
        .min(finalized_l1.number.0);
    Ok(get_block(client, l1_rpc, BlockTag::Number(target))?.hash)
}

fn l1_origin_number(client: &Client, rpc_url: &str, block_number: u64) -> eyre::Result<u64> {
    let selector = &keccak256("number()")[..4];
    let result: String = rpc(
        client,
        rpc_url,
        "eth_call",
        json!([
            {
                "to": format!("{L1_BLOCK_PREDEPLOY:#x}"),
                "data": format!("0x{}", hex::encode(selector)),
            },
            BlockTag::Number(block_number).to_rpc_value(),
        ]),
    )?
    .context("eth_call to L1Block.number() returned null")?;

    parse_hex_u64_word(&result)
        .wrap_err_with(|| format!("failed to parse L1Block.number() result {result}"))
}

fn parse_hex_u64_word(value: &str) -> eyre::Result<u64> {
    let value = value
        .strip_prefix("0x")
        .context("hex quantity must start with 0x")?
        .trim_start_matches('0');
    if value.is_empty() {
        return Ok(0);
    }
    Ok(u64::from_str_radix(value, 16)?)
}

fn rpc<T: DeserializeOwned>(
    client: &Client,
    rpc_url: &str,
    method: &str,
    params: Value,
) -> eyre::Result<Option<T>> {
    let response: RpcResponse<T> = client
        .post(rpc_url)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        }))
        .send()
        .wrap_err_with(|| format!("failed to call {method}"))?
        .error_for_status()
        .wrap_err_with(|| format!("{method} returned HTTP error"))?
        .json()
        .wrap_err_with(|| format!("failed to decode {method} response"))?;

    if let Some(error) = response.error {
        return Err(eyre!(
            "{method} RPC error {}: {}",
            error.code,
            error.message
        ));
    }

    Ok(response.result)
}

#[derive(Clone, Copy, Debug)]
enum BlockTag {
    Number(u64),
    Finalized,
}

impl BlockTag {
    fn to_rpc_value(self) -> String {
        match self {
            Self::Number(n) => format!("0x{n:x}"),
            Self::Finalized => "finalized".to_string(),
        }
    }

    fn display(self) -> String {
        self.to_rpc_value()
    }
}

fn witness_bytes(witness: &WorldRangeWitnessData) -> eyre::Result<Vec<u8>> {
    Ok(rkyv::to_bytes::<rkyv::rancor::Error>(witness)?.to_vec())
}

fn write_json(path: &Path, value: &impl Serialize) -> eyre::Result<()> {
    ensure_parent_dir(path)?;
    fs::write(path, serde_json::to_vec_pretty(value)?)
        .wrap_err_with(|| format!("failed to write {}", path.display()))
}

fn write_bytes(path: &Path, value: &[u8]) -> eyre::Result<()> {
    ensure_parent_dir(path)?;
    fs::write(path, value).wrap_err_with(|| format!("failed to write {}", path.display()))
}

fn sibling_path(base: &Path, suffix: &str) -> PathBuf {
    let stem = base
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("witness");
    base.with_file_name(format!("{stem}.{suffix}"))
}

fn trim_rpc_url(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

fn ensure_parent_dir(path: &Path) -> eyre::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
    }
    Ok(())
}

#[cfg(feature = "sp1")]
fn sp1_execute(args: Sp1ExecuteArgs) -> eyre::Result<()> {
    use sp1_sdk::{Prover, ProverClient, SP1Stdin};

    let witness_bytes = fs::read(&args.witness)
        .wrap_err_with(|| format!("failed to read {}", args.witness.display()))?;
    let elf = fs::read(&args.elf)
        .wrap_err_with(|| format!("failed to read ELF {}", args.elf.display()))?;

    let mut stdin = SP1Stdin::new();
    stdin.write_vec(witness_bytes);

    tokio::runtime::Runtime::new()?.block_on(async {
        let client = ProverClient::builder().cpu().build().await;
        let (public_values, report) = client
            .execute(elf.into(), stdin)
            .await
            .map_err(|e| eyre!("SP1 execution failed: {e}"))?;

        println!("execution succeeded");
        println!("total cycles:  {}", report.total_instruction_count());
        println!("public values: 0x{}", hex::encode(public_values.as_slice()));
        Ok(())
    })
}

#[cfg(feature = "sp1")]
fn sp1_vkeys(args: Sp1VkeysArgs) -> eyre::Result<()> {
    use sha2::{Digest, Sha256};
    use sp1_sdk::{CpuProver, HashableKey, Prover, ProvingKey, env::EnvProver};
    use world_chain_proof_core::types::u32_to_u8;

    let range_elf = fs::read(&args.range_elf)
        .wrap_err_with(|| format!("failed to read ELF {}", args.range_elf.display()))?;
    let agg_elf = fs::read(&args.agg_elf)
        .wrap_err_with(|| format!("failed to read ELF {}", args.agg_elf.display()))?;

    let range_elf_sha256 = hex::encode(Sha256::digest(&range_elf));
    let agg_elf_sha256 = hex::encode(Sha256::digest(&agg_elf));

    let (range_vkey_commitment, aggregation_vkey) =
        tokio::runtime::Runtime::new()?.block_on(async {
            let client = EnvProver::Cpu(CpuProver::new().await);
            let range_pk = client
                .setup(range_elf.into())
                .await
                .map_err(|e| eyre!("range setup failed: {e}"))?;
            let agg_pk = client
                .setup(agg_elf.into())
                .await
                .map_err(|e| eyre!("aggregation setup failed: {e}"))?;
            let range_vkey_commitment = B256::from(u32_to_u8(range_pk.verifying_key().hash_u32()));
            let aggregation_vkey = agg_pk.verifying_key().bytes32();
            Ok::<_, eyre::Report>((range_vkey_commitment, aggregation_vkey))
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
            fs::write(path, &out)
                .wrap_err_with(|| format!("failed to write {}", path.display()))?;
            println!("wrote vkeys to {}", path.display());
        }
        None => println!("{out}"),
    }
    Ok(())
}

#[cfg(feature = "sp1")]
fn fetch_l1_header_by_hash(
    client: &Client,
    rpc_url: &str,
    hash: B256,
) -> eyre::Result<alloy_consensus::Header> {
    let block_json: serde_json::Value =
        rpc(client, rpc_url, "eth_getBlockByHash", json!([hash, false]))?
            .with_context(|| format!("eth_getBlockByHash returned null for {hash:?}"))?;
    serde_json::from_value(block_json).wrap_err("failed to deserialize L1 block header")
}

#[cfg(feature = "sp1")]
fn sp1_prove(args: Sp1ProveArgs) -> eyre::Result<()> {
    use alloy_consensus::Header as L1Header;
    use sp1_sdk::{HashableKey, Prover, ProverClient, ProvingKey, SP1Stdin};
    use world_chain_proof_core::{boot::BootInfoStruct, types::AggregationInputs};

    let n_ranges = args.ranges.max(1);
    let total_blocks = args
        .rpc
        .end_block
        .checked_sub(args.rpc.start_block)
        .filter(|&n| n > 0)
        .ok_or_else(|| eyre!("end_block must be > start_block"))?;
    if n_ranges > total_blocks {
        return Err(eyre!(
            "--ranges {n_ranges} exceeds total block count {total_blocks}"
        ));
    }

    let http_client = Client::new();

    // Resolve a single L1 head that covers all sub-ranges.
    let l1_head_hash = match args.rpc.l1_head {
        Some(h) => h,
        None => resolve_l1_head(
            &http_client,
            &args.rpc.l2_rpc,
            &args.rpc.l1_rpc,
            args.rpc.end_block,
        )?,
    };

    // Compute sub-range boundaries: [start, end] with end > start.
    let sub_ranges: Vec<(u64, u64)> = (0..n_ranges)
        .map(|i| {
            let start = args.rpc.start_block + total_blocks * i / n_ranges;
            let end = args.rpc.start_block + total_blocks * (i + 1) / n_ranges;
            (start, end)
        })
        .collect();

    // Build all witnesses synchronously before entering the async proving loop.
    let mut range_inputs: Vec<RangeProofInput> = Vec::with_capacity(sub_ranges.len());
    for (i, (sub_start, sub_end)) in sub_ranges.iter().enumerate() {
        println!(
            "range {}/{}: generating witness for blocks {}..={}",
            i + 1,
            n_ranges,
            sub_start + 1,
            sub_end,
        );
        let mut sub_rpc = args.rpc.clone();
        sub_rpc.start_block = *sub_start;
        sub_rpc.end_block = *sub_end;
        sub_rpc.l1_head = Some(l1_head_hash);
        range_inputs.push(build_range_input(&sub_rpc)?);
    }

    // Fetch the single L1 header for the shared checkpoint head and CBOR-encode it.
    let l1_header: L1Header =
        fetch_l1_header_by_hash(&http_client, &args.rpc.l1_rpc, l1_head_hash)?;
    let headers_cbor = serde_cbor::to_vec(&vec![l1_header])
        .map_err(|e| eyre!("CBOR-encoding L1 header failed: {e}"))?;

    let range_elf = fs::read(&args.range_elf)
        .wrap_err_with(|| format!("failed to read {}", args.range_elf.display()))?;
    let agg_elf = fs::read(&args.agg_elf)
        .wrap_err_with(|| format!("failed to read {}", args.agg_elf.display()))?;
    let prover_address = args.prover_address;
    let output = args.output;

    let sp1_prover = args.prover;
    let sp1_mode = args.mode;

    tokio::runtime::Runtime::new()?.block_on(async {
        use sp1_sdk::{
            CpuProver, MockProver, ProveRequest, SP1Proof, SP1ProofMode, env::EnvProver,
        };
        let client: EnvProver = match sp1_prover {
            Sp1Prover::Cpu => EnvProver::Cpu(CpuProver::new().await),
            Sp1Prover::Mock => EnvProver::Mock(MockProver::new().await),
            Sp1Prover::Network => {
                EnvProver::Network(ProverClient::builder().network().build().await)
            }
        };

        let range_pk = client
            .setup(range_elf.into())
            .await
            .map_err(|e| eyre!("range setup failed: {e}"))?;
        let multi_block_vkey = range_pk.verifying_key().hash_u32();

        // Spawn all range proofs in parallel. Range proofs must be Compressed so
        // the aggregation guest can recursively verify them with verify_sp1_proof.
        let mut handles = Vec::with_capacity(range_inputs.len());
        for (i, input) in range_inputs.iter().enumerate() {
            let client = client.clone();
            let range_pk = range_pk.clone();
            let bytes = witness_bytes(&input.witness)?;
            let start = input.metadata.start_block;
            let end = input.metadata.end_block;
            handles.push(tokio::spawn(async move {
                println!(
                    "range {}/{n_ranges}: proving blocks {}..={}...",
                    i + 1,
                    start + 1,
                    end
                );
                let mut stdin = SP1Stdin::new();
                stdin.write_vec(bytes);
                let proof = client
                    .prove(&range_pk, stdin)
                    .compressed()
                    .await
                    .map_err(|e| eyre!("range proof {i} failed: {e}"))?;
                let boot_info: BootInfoStruct =
                    bincode::deserialize(proof.public_values.as_slice()).map_err(|e| {
                        eyre!("range {i} public values deserialization failed: {e}")
                    })?;
                println!(
                    "  range {}: block {} pre={:?} post={:?}",
                    i + 1,
                    boot_info.l2BlockNumber,
                    boot_info.l2PreRoot,
                    boot_info.l2PostRoot,
                );
                Ok::<_, eyre::Report>((boot_info, proof))
            }));
        }

        let mut boot_infos: Vec<BootInfoStruct> = Vec::with_capacity(handles.len());
        let mut compressed_range_proofs = Vec::with_capacity(handles.len());
        for (i, handle) in handles.into_iter().enumerate() {
            let (boot_info, proof) = handle
                .await
                .map_err(|e| eyre!("range task {i} panicked: {e}"))??;
            boot_infos.push(boot_info);
            compressed_range_proofs.push(proof);
        }

        let agg_inputs = AggregationInputs {
            boot_infos,
            latest_l1_checkpoint_head: l1_head_hash,
            multi_block_vkey,
            prover_address,
        };

        let agg_pk = client
            .setup(agg_elf.into())
            .await
            .map_err(|e| eyre!("aggregation setup failed: {e}"))?;

        let mut agg_stdin = SP1Stdin::new();
        // Write each compressed range proof as a deferred proof so the aggregation
        // guest's verify_sp1_proof calls have something to verify against.
        // Order must match boot_infos order.
        for proof in compressed_range_proofs {
            let SP1Proof::Compressed(inner) = proof.proof else {
                return Err(eyre!("range proof was not Compressed"));
            };
            agg_stdin.write_proof(*inner, range_pk.verifying_key().vk.clone());
        }
        agg_stdin.write(&agg_inputs);
        agg_stdin.write_vec(headers_cbor);

        let agg_proof_mode = match sp1_mode {
            Sp1Mode::Core => SP1ProofMode::Core,
            Sp1Mode::Compressed => SP1ProofMode::Compressed,
            Sp1Mode::Plonk => SP1ProofMode::Plonk,
            Sp1Mode::Groth16 => SP1ProofMode::Groth16,
        };

        println!("generating aggregation proof over {n_ranges} range(s) ({sp1_mode:?} mode)...");
        let mut agg_prove = client.prove(&agg_pk, agg_stdin).mode(agg_proof_mode);
        if matches!(sp1_prover, Sp1Prover::Mock) {
            // Mock proofs are dummies; skip recursive verification inside the guest.
            agg_prove = agg_prove.deferred_proof_verification(false);
        }
        let agg_proof = agg_prove
            .await
            .map_err(|e| eyre!("aggregation proof failed: {e}"))?;

        if matches!(sp1_prover, Sp1Prover::Mock) {
            println!("skipping verification (mock prover)");
        } else {
            client
                .verify(&agg_proof, agg_pk.verifying_key(), None)
                .map_err(|e| eyre!("aggregation proof verification failed: {e}"))?;
            println!("aggregation proof verified OK");
        }

        if let Some(path) = output {
            write_bytes(&path, &serde_json::to_vec_pretty(&agg_proof)?)?;
            println!("proof written to {}", path.display());
        }

        Ok(())
    })
}
