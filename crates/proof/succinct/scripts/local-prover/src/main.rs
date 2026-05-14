use std::{
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy_primitives::{Address, B256, keccak256};
use clap::{Args, Parser, Subcommand, ValueEnum};
use color_eyre::eyre::{self, Context, ContextCompat, eyre};
use kona_host::{DataFormat, single::SingleChainHost};
use kona_preimage::{BidirectionalChannel, HintWriter, NativeChannel, OracleReader};
use kona_proof::{CachingOracle, l1::OracleBlobProvider};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
#[cfg(feature = "sp1")]
use sp1_sdk::{
    HashableKey, ProvingKey,
    blocking::{Elf, ProveRequest, Prover, ProverClient, SP1ProofMode, SP1Stdin},
};
use world_chain_chainspec::WorldChainSpec;
use world_chain_proof_protocol::{
    WorldHardforkConfig as ProtocolHardforkConfig, hash_rollup_config,
};
use world_chain_proof_succinct_client_utils::{
    WorldRangeHardforkConfig, WorldRangeSpecId,
    boot::{BootInfoPublicValues, BootInfoStruct},
    range::OutputRootWitness,
    witness::{
        BlobData, WorldRangeWitnessData,
        executor::{WitnessExecutor, get_inputs_for_pipeline},
        preimage_store::PreimageStore,
    },
};
use world_chain_proof_succinct_ethereum_client_utils::executor::ETHDAWitnessExecutor;
use world_chain_proof_succinct_host_utils::witness_generation::{
    OnlineBlobStore, PreimageWitnessCollector,
};

const L1_BLOCK_PREDEPLOY: Address = Address::new([
    0x42, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x15,
]);
const L2_TO_L1_MESSAGE_PASSER: Address = Address::new([
    0x42, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x16,
]);

type DefaultOracleBase = CachingOracle<OracleReader<NativeChannel>, HintWriter<NativeChannel>>;
type WorldPreimageCollector = PreimageWitnessCollector<DefaultOracleBase>;
type WorldOnlineBlobStore = OnlineBlobStore<OracleBlobProvider<DefaultOracleBase>>;
type WorldEthWitnessExecutor = ETHDAWitnessExecutor<WorldPreimageCollector, WorldOnlineBlobStore>;

#[derive(Debug, Parser)]
#[command(
    name = "world-chain-proof-succinct-local-prover",
    about = "Local World Chain mainnet SP1 proof runner"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build and write the mainnet one-block witness without proving.
    Witness(BlockArgs),
    /// Prove a World Chain mainnet block locally with the range ELF.
    Prove(ProveArgs),
    /// Print local verifying key metadata for compiled ELFs.
    Vkeys(VkeyArgs),
}

#[derive(Debug, Args)]
struct BlockArgs {
    /// L2 block number to prove.
    #[arg(long)]
    block: u64,

    /// World Chain mainnet L2 execution RPC URL.
    #[arg(long, env = "WORLD_CHAIN_MAINNET_L2_RPC_URL")]
    l2_rpc: String,

    /// Ethereum L1 execution RPC URL.
    #[arg(long, env = "WORLD_CHAIN_MAINNET_L1_RPC_URL")]
    l1_rpc: String,

    /// Ethereum L1 beacon API URL.
    #[arg(long, env = "WORLD_CHAIN_MAINNET_L1_BEACON_RPC_URL")]
    l1_beacon_rpc: String,

    /// World Chain mainnet rollup config JSON file.
    #[arg(long, env = "WORLD_CHAIN_MAINNET_ROLLUP_CONFIG")]
    rollup_config: Option<PathBuf>,

    /// Rollup config hash to use when no rollup config file is supplied.
    #[arg(long, env = "WORLD_CHAIN_MAINNET_ROLLUP_CONFIG_HASH")]
    rollup_config_hash: Option<B256>,

    /// Optional override for the L1 origin hash committed in public values.
    #[arg(long, env = "WORLD_CHAIN_MAINNET_L1_HEAD")]
    l1_head: Option<B256>,

    /// Allow proving a block newer than the finalized L2 head.
    #[arg(long)]
    allow_unfinalized: bool,

    /// Maximum time to spend generating the Kona witness.
    #[arg(long, default_value_t = 900)]
    witness_timeout_seconds: u64,

    /// Output rkyv witness bytes file.
    #[arg(long)]
    output: PathBuf,
}

#[derive(Debug, Args)]
struct ProveArgs {
    #[command(flatten)]
    block: BlockSharedArgs,

    /// Compiled World range guest ELF.
    #[arg(
        long,
        env = "WORLD_CHAIN_RANGE_ELF",
        default_value = "crates/proof/succinct/elf/world-chain-range-ethereum"
    )]
    range_elf: PathBuf,

    /// Proof mode.
    #[arg(long, value_enum, default_value_t = ProveMode::Core)]
    mode: ProveMode,

    /// Output proof file.
    #[arg(long)]
    output: PathBuf,
}

#[derive(Debug, Args)]
struct BlockSharedArgs {
    /// L2 block number to prove.
    #[arg(long)]
    block: u64,

    /// World Chain mainnet L2 execution RPC URL.
    #[arg(long, env = "WORLD_CHAIN_MAINNET_L2_RPC_URL")]
    l2_rpc: String,

    /// Ethereum L1 execution RPC URL.
    #[arg(long, env = "WORLD_CHAIN_MAINNET_L1_RPC_URL")]
    l1_rpc: String,

    /// Ethereum L1 beacon API URL.
    #[arg(long, env = "WORLD_CHAIN_MAINNET_L1_BEACON_RPC_URL")]
    l1_beacon_rpc: String,

    /// World Chain mainnet rollup config JSON file.
    #[arg(long, env = "WORLD_CHAIN_MAINNET_ROLLUP_CONFIG")]
    rollup_config: Option<PathBuf>,

    /// Rollup config hash to use when no rollup config file is supplied.
    #[arg(long, env = "WORLD_CHAIN_MAINNET_ROLLUP_CONFIG_HASH")]
    rollup_config_hash: Option<B256>,

    /// Optional override for the L1 origin hash committed in public values.
    #[arg(long, env = "WORLD_CHAIN_MAINNET_L1_HEAD")]
    l1_head: Option<B256>,

    /// Allow proving a block newer than the finalized L2 head.
    #[arg(long)]
    allow_unfinalized: bool,

    /// Maximum time to spend generating the Kona witness.
    #[arg(long, default_value_t = 900)]
    witness_timeout_seconds: u64,
}

#[derive(Debug, Args)]
struct VkeyArgs {
    /// Compiled World range guest ELF.
    #[arg(
        long,
        env = "WORLD_CHAIN_RANGE_ELF",
        default_value = "crates/proof/succinct/elf/world-chain-range-ethereum"
    )]
    range_elf: PathBuf,

    /// Optional compiled World aggregation guest ELF.
    #[arg(long, env = "WORLD_CHAIN_AGGREGATION_ELF")]
    aggregation_elf: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum ProveMode {
    /// Generate a core SP1 proof.
    Core,
    /// Generate a compressed SP1 proof.
    Compressed,
    /// Generate a PLONK SP1 proof.
    Plonk,
    /// Generate a Groth16 SP1 proof.
    Groth16,
}

#[cfg(feature = "sp1")]
impl From<ProveMode> for SP1ProofMode {
    fn from(value: ProveMode) -> Self {
        match value {
            ProveMode::Core => Self::Core,
            ProveMode::Compressed => Self::Compressed,
            ProveMode::Plonk => Self::Plonk,
            ProveMode::Groth16 => Self::Groth16,
        }
    }
}

struct LocalBlockProofInput {
    metadata: LocalProofMetadata,
    witness: WorldRangeWitnessData,
    boot_info: BootInfoStruct,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalProofMetadata {
    block: u64,
    finalized_l2_head: Option<u64>,
    l1_head: B256,
    l2_pre_root: B256,
    l2_post_root: B256,
    l2_block_number: u64,
    l2_timestamp: u64,
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
        Command::Witness(args) => {
            let input = build_local_input(
                args.block,
                &args.l2_rpc,
                &args.l1_rpc,
                &args.l1_beacon_rpc,
                args.rollup_config.as_deref(),
                args.rollup_config_hash,
                args.l1_head,
                args.allow_unfinalized,
                args.witness_timeout_seconds,
            )?;
            write_bytes(&args.output, &witness_bytes(&input.witness)?)?;
            let metadata_path = metadata_path(&args.output);
            write_json(
                &metadata_path,
                &json!({
                    "metadata": input.metadata,
                    "bootInfo": input.boot_info,
                }),
            )?;
            println!("wrote witness bytes to {}", args.output.display());
            println!("wrote metadata to {}", metadata_path.display());
        }
        Command::Prove(args) => prove(args)?,
        Command::Vkeys(args) => print_vkeys(args)?,
    }

    Ok(())
}

#[cfg(feature = "sp1")]
fn prove(args: ProveArgs) -> eyre::Result<()> {
    let input = build_local_input(
        args.block.block,
        &args.block.l2_rpc,
        &args.block.l1_rpc,
        &args.block.l1_beacon_rpc,
        args.block.rollup_config.as_deref(),
        args.block.rollup_config_hash,
        args.block.l1_head,
        args.block.allow_unfinalized,
        args.block.witness_timeout_seconds,
    )?;
    ensure_parent_dir(&args.output)?;

    let elf = fs::read(&args.range_elf)
        .wrap_err_with(|| format!("failed to read {}", args.range_elf.display()))?;
    let prover = ProverClient::builder().cpu().build();
    let pk = prover.setup(Elf::from(elf))?;

    let mut stdin = SP1Stdin::new();
    stdin.write_slice(&witness_bytes(&input.witness)?);

    let proof = prover
        .prove(&pk, stdin)
        .mode(SP1ProofMode::from(args.mode))
        .run()?;
    let vk = pk.verifying_key();
    prover.verify(&proof, vk, None)?;
    println!("verified {:?} proof with local SP1 verifier", args.mode);
    proof
        .save(&args.output)
        .map_err(|err| eyre!("failed to save proof to {}: {err}", args.output.display()))?;

    let metadata_path = metadata_path(&args.output);
    write_json(
        &metadata_path,
        &json!({
            "metadata": input.metadata,
            "bootInfo": input.boot_info,
            "rangeVkey": vk.bytes32(),
        }),
    )?;

    println!("wrote proof to {}", args.output.display());
    println!("wrote metadata to {}", metadata_path.display());
    println!("range vkey: {}", vk.bytes32());

    Ok(())
}

#[cfg(not(feature = "sp1"))]
fn prove(_args: ProveArgs) -> eyre::Result<()> {
    Err(eyre!(
        "the prove command requires `--features sp1`; on macOS install protobuf first with `brew install protobuf`"
    ))
}

#[cfg(feature = "sp1")]
fn print_vkeys(args: VkeyArgs) -> eyre::Result<()> {
    let prover = ProverClient::builder().cpu().build();
    let range_elf = fs::read(&args.range_elf)
        .wrap_err_with(|| format!("failed to read {}", args.range_elf.display()))?;
    let range_pk = prover.setup(Elf::from(range_elf))?;

    let aggregation_vkey = if let Some(path) = args.aggregation_elf {
        let elf = fs::read(&path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
        let pk = prover.setup(Elf::from(elf))?;
        Some(pk.verifying_key().bytes32())
    } else {
        None
    };

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "rangeVkey": range_pk.verifying_key().bytes32(),
            "aggregationVkey": aggregation_vkey,
        }))?
    );
    Ok(())
}

#[cfg(not(feature = "sp1"))]
fn print_vkeys(_args: VkeyArgs) -> eyre::Result<()> {
    Err(eyre!(
        "the vkeys command requires `--features sp1`; on macOS install protobuf first with `brew install protobuf`"
    ))
}

fn build_local_input(
    block_number: u64,
    l2_rpc: &str,
    l1_rpc: &str,
    l1_beacon_rpc: &str,
    rollup_config_path: Option<&Path>,
    rollup_config_hash: Option<B256>,
    l1_head_override: Option<B256>,
    allow_unfinalized: bool,
    witness_timeout_seconds: u64,
) -> eyre::Result<LocalBlockProofInput> {
    if block_number == 0 {
        return Err(eyre!("block 0 cannot be proven as a one-block transition"));
    }

    let client = Client::new();
    let finalized_l2_head = finalized_l2_head(&client, l2_rpc)?;
    if !allow_unfinalized && finalized_l2_head.is_some_and(|head| block_number > head) {
        return Err(eyre!(
            "block {block_number} is newer than finalized L2 head {}; pass --allow-unfinalized to override",
            finalized_l2_head.unwrap()
        ));
    }

    let (schedule, rollup_config_hash) = proof_config(rollup_config_path, rollup_config_hash)?;
    let pre_block = get_block(&client, l2_rpc, BlockTag::Number(block_number - 1))?;
    let post_block = get_block(&client, l2_rpc, BlockTag::Number(block_number))?;
    let pre_state = output_root_witness(&client, l2_rpc, block_number - 1, &pre_block)?;
    let post_state = output_root_witness(&client, l2_rpc, block_number, &post_block)?;
    let pre_root = pre_state.output_root();
    let post_root = post_state.output_root();
    let l1_head = match l1_head_override {
        Some(hash) => hash,
        None => resolve_l1_head(&client, l2_rpc, l1_rpc, block_number)?,
    };

    let boot_info = BootInfoStruct::from(BootInfoPublicValues::new(
        l1_head,
        pre_root,
        post_root,
        block_number,
        rollup_config_hash,
    ));
    let active_fork = schedule.active_fork_at(block_number, post_block.timestamp.0);
    let world_spec_id = WorldRangeSpecId::from_hardfork(active_fork);

    let host = SingleChainHost {
        l1_head,
        agreed_l2_head_hash: pre_block.hash,
        agreed_l2_output_root: pre_root,
        claimed_l2_output_root: post_root,
        claimed_l2_block_number: block_number,
        l2_node_address: Some(trim_rpc_url(l2_rpc)),
        l1_node_address: Some(trim_rpc_url(l1_rpc)),
        l1_beacon_address: Some(trim_rpc_url(l1_beacon_rpc)),
        data_dir: None,
        data_format: DataFormat::default(),
        native: false,
        server: true,
        l2_chain_id: rollup_config_path.is_none().then_some(480),
        rollup_config_path: rollup_config_path.map(Path::to_path_buf),
        l1_config_path: None,
        enable_experimental_witness_endpoint: false,
    };

    let witness = tokio::runtime::Runtime::new()?.block_on(collect_world_range_witness(
        host,
        schedule.clone(),
        rollup_config_hash,
        Duration::from_secs(witness_timeout_seconds),
    ))?;

    Ok(LocalBlockProofInput {
        metadata: LocalProofMetadata {
            block: block_number,
            finalized_l2_head,
            l1_head,
            l2_pre_root: pre_root,
            l2_post_root: post_root,
            l2_block_number: block_number,
            l2_timestamp: post_block.timestamp.0,
            rollup_config_hash,
            active_fork: format!("{active_fork:?}"),
            world_spec_id: <&'static str>::from(world_spec_id).to_string(),
        },
        witness,
        boot_info,
    })
}

async fn collect_world_range_witness(
    host: SingleChainHost,
    schedule: WorldRangeHardforkConfig,
    rollup_config_hash: B256,
    timeout: Duration,
) -> eyre::Result<WorldRangeWitnessData> {
    let preimage = BidirectionalChannel::new()?;
    let hint = BidirectionalChannel::new()?;
    let mut server_task = host.start_server(hint.host, preimage.host).await?;

    let witness = collect_world_range_witness_from_channels(
        preimage.client,
        hint.client,
        schedule,
        rollup_config_hash,
    );
    tokio::pin!(witness);

    let result = match tokio::time::timeout(timeout, async {
        tokio::select! {
            result = &mut witness => result,
            server_result = &mut server_task => match server_result {
                Ok(Ok(())) => Err(eyre!("Kona preimage server exited before witness generation completed")),
                Ok(Err(err)) => Err(eyre!("Kona preimage server failed: {err}")),
                Err(err) => Err(eyre!("Kona preimage server task failed: {err}")),
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

async fn collect_world_range_witness_from_channels(
    preimage_chan: NativeChannel,
    hint_chan: NativeChannel,
    schedule: WorldRangeHardforkConfig,
    rollup_config_hash: B256,
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
                l1_provider.clone(),
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
        rollup_config_hash,
    ))
}

fn proof_config(
    rollup_config_path: Option<&Path>,
    rollup_config_hash: Option<B256>,
) -> eyre::Result<(WorldRangeHardforkConfig, B256)> {
    if let Some(path) = rollup_config_path {
        return proof_config_from_file(path);
    }

    let hash = rollup_config_hash.context(
        "provide --rollup-config or WORLD_CHAIN_MAINNET_ROLLUP_CONFIG, or provide --rollup-config-hash",
    )?;
    let spec = WorldChainSpec::mainnet();
    Ok((
        range_hardfork_config(&ProtocolHardforkConfig::from_chain_spec(spec.as_ref())),
        hash,
    ))
}

fn proof_config_from_file(path: &Path) -> eyre::Result<(WorldRangeHardforkConfig, B256)> {
    let bytes = fs::read(path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .wrap_err_with(|| format!("failed to parse {}", path.display()))?;
    let schedule = ProtocolHardforkConfig::from_rollup_config_value(&value)?;
    let hash = hash_rollup_config(&value)?;
    Ok((range_hardfork_config(&schedule), hash))
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
    .map(|block| block.number.0))
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
            "optimism_outputAtBlock output root mismatch for block {block_number}: computed {computed}, RPC returned {}",
            output.output_root
        ));
    }
    Ok(witness)
}

fn resolve_l1_head(
    client: &Client,
    l2_rpc: &str,
    l1_rpc: &str,
    block_number: u64,
) -> eyre::Result<B256> {
    let l1_origin_number = l1_origin_number(client, l2_rpc, block_number)?;
    let finalized_l1 = get_block(client, l1_rpc, BlockTag::Finalized)?;
    let target_l1_number = l1_origin_number
        .saturating_add(20)
        .min(finalized_l1.number.0);
    Ok(get_block(client, l1_rpc, BlockTag::Number(target_l1_number))?.hash)
}

#[allow(dead_code)]
fn l1_origin_hash(client: &Client, rpc_url: &str, block_number: u64) -> eyre::Result<B256> {
    let selector = &keccak256("hash()")[..4];
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
    .context("eth_call to L1Block.hash() returned null")?;

    B256::from_str(&result)
        .wrap_err_with(|| format!("failed to parse L1Block.hash() result {result}"))
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
            Self::Number(number) => format!("0x{number:x}"),
            Self::Finalized => "finalized".to_string(),
        }
    }

    fn display(self) -> String {
        self.to_rpc_value()
    }
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

fn witness_bytes(witness: &WorldRangeWitnessData) -> eyre::Result<Vec<u8>> {
    Ok(rkyv::to_bytes::<rkyv::rancor::Error>(witness)?.to_vec())
}

fn trim_rpc_url(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

fn ensure_parent_dir(path: &Path) -> eyre::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
    }
    Ok(())
}

fn metadata_path(output: &Path) -> PathBuf {
    let stem = output
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("proof");
    output.with_file_name(format!("{stem}.metadata.json"))
}
