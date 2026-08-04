//! Online (RPC-backed) construction of World Chain range-proof witnesses.
//!
//! Builds the rkyv-serializable [`WorldRangeWitnessData`] for a block range by driving the
//! Kona single-chain host against live L1/L2 RPC endpoints. Extracted from the `proof` CLI so
//! long-running services (the `sp1-worker`, `nitro-worker`) and the CLI share one implementation.
//!
//! The entry point [`build_range_input`] is synchronous and spins up its own Tokio runtime;
//! it MUST NOT be called from within an async context (use `tokio::task::spawn_blocking`).

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::{OnlineBlobStore, PreimageWitnessCollector};
use alloy_primitives::{Address, B256, address, keccak256};
use anyhow::{Context, anyhow, bail};
use kona_host::{DataFormat, single::SingleChainHost};
use kona_preimage::{BidirectionalChannel, HintWriter, NativeChannel, OracleReader};
use kona_proof::{CachingOracle, l1::OracleBlobProvider};
use reqwest::Client;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tracing::warn;
use world_chain_chainspec::{WorldChainHardfork, WorldChainHardforks};
use world_chain_proof_core::{
    hash_world_rollup_config,
    range::{WorldRangeHardfork, WorldRangeHardforkConfig, WorldRangeSpecId},
    witness::{BlobData, WorldRangeWitnessData, preimage_store::PreimageStore},
};
use world_chain_proof_kona_client_utils::{
    ETHDAWitnessExecutor, OutputRootWitness, WitnessExecutor, get_inputs_for_pipeline,
};

const L1_BLOCK_PREDEPLOY: Address = address!("0x4200000000000000000000000000000000000015");
const L2_TO_L1_MESSAGE_PASSER: Address = address!("0x4200000000000000000000000000000000000016");

/// Per-request timeout for every RPC call made while assembling a witness.
///
/// Generous enough for the pre-Isthmus `eth_getProof` fallback, which rebuilds historical state on
/// the serving node, but bounded so a hung endpoint fails the job instead of stalling the worker.
const RPC_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

type DefaultOracleBase = CachingOracle<OracleReader<NativeChannel>, HintWriter<NativeChannel>>;
type WorldPreimageCollector = PreimageWitnessCollector<DefaultOracleBase>;
type WorldOnlineBlobStore = OnlineBlobStore<OracleBlobProvider<DefaultOracleBase>>;
type WorldEthWitnessExecutor = ETHDAWitnessExecutor<WorldPreimageCollector, WorldOnlineBlobStore>;

/// RPC endpoints and proof configuration shared by all witness builds for one chain.
#[derive(Clone, Debug)]
pub struct OnlineHostConfig {
    /// Ethereum L1 execution RPC URL.
    pub l1_rpc: String,
    /// Ethereum L1 beacon API URL.
    pub l1_beacon_rpc: String,
    /// World Chain L2 execution RPC URL.
    pub l2_rpc: String,
    /// World hardfork schedule baked into the witness.
    pub schedule: WorldRangeHardforkConfig,
    /// Rollup config hash recorded in the witness metadata.
    pub rollup_config_hash: B256,
    /// L2 chain id resolving kona's bundled rollup config. Ignored when `rollup_config_path`
    /// is set.
    pub l2_chain_id: Option<u64>,
    /// Rollup config JSON file consumed by the kona host, for chains kona does not bundle.
    pub rollup_config_path: Option<PathBuf>,
    /// Maximum time for one witness, covering both the RPC prologue and the Kona host.
    pub witness_timeout: Duration,
}

impl OnlineHostConfig {
    /// Builds a host config from a rollup config JSON value, deriving the World fork schedule
    /// and rollup config hash from it. The same JSON should be written to `rollup_config_path`
    /// so the kona host and the witness collector agree.
    ///
    /// # Rollup config hash canonicalization
    ///
    /// The hash committed here MUST be computed the same way the enclave/guest computes
    /// [`TransitionPublicValues::rollupConfigHash`] (via [`hash_world_rollup_config`] over a
    /// parsed [`kona_genesis::RollupConfig`]), not a hash of the raw input JSON text.
    ///
    /// Kona's `RollupConfig` fills in defaults for fields that are optional-with-a-default in
    /// the Rust struct (e.g. `granite_channel_timeout`) even when they are absent from the
    /// source JSON, and serializes struct fields in declaration order regardless of the
    /// source JSON's key order. The enclave only ever sees a `RollupConfig` reconstructed from
    /// Kona's own (de)serialization, so hashing the *raw* JSON text here (as opposed to hashing
    /// the same round-tripped `RollupConfig`) silently diverges from the enclave's hash
    /// whenever the source rollup.json isn't already in that exact canonical form — e.g. any
    /// rollup.json that omits `granite_channel_timeout`, or isn't ordered/produced by Kona's own
    /// serializer (such as one emitted by op-node instead of kona-node). This previously caused
    /// spurious "enclave rollup config hash != expected" failures on chains whose rollup.json
    /// wasn't authored by Kona (see World Chain alphanet's kona-node -> op-node migration).
    pub fn from_rollup_config_value(
        rollup_config: &serde_json::Value,
        l1_rpc: String,
        l1_beacon_rpc: String,
        l2_rpc: String,
        rollup_config_path: Option<PathBuf>,
        witness_timeout: Duration,
    ) -> anyhow::Result<Self> {
        let schedule: WorldRangeHardforkConfig = serde_json::from_value(rollup_config.clone())
            .context("failed to parse rollup config hardforks")?;
        let parsed_rollup_config: kona_genesis::RollupConfig =
            serde_json::from_value(rollup_config.clone())
                .context("failed to parse rollup config")?;
        let rollup_config_hash = hash_world_rollup_config(&parsed_rollup_config, &schedule)
            .context("failed to hash rollup config")?;
        Ok(Self {
            l1_rpc,
            l1_beacon_rpc,
            l2_rpc,
            schedule,
            rollup_config_hash,
            l2_chain_id: None,
            rollup_config_path,
            witness_timeout,
        })
    }
}

/// Builds the range guest's hardfork schedule from a World chain spec.
pub fn hardfork_config_from_chain_spec<S>(chain_spec: &S) -> WorldRangeHardforkConfig
where
    S: WorldChainHardforks + ?Sized,
{
    let block_number = |fork| chain_spec.world_chain_fork_activation(fork).block_number();
    let timestamp = |fork| chain_spec.world_chain_fork_activation(fork).as_timestamp();

    WorldRangeHardforkConfig {
        bedrock_block: block_number(WorldChainHardfork::Bedrock),
        regolith_time: timestamp(WorldChainHardfork::Regolith),
        canyon_time: timestamp(WorldChainHardfork::Canyon),
        ecotone_time: timestamp(WorldChainHardfork::Ecotone),
        fjord_time: timestamp(WorldChainHardfork::Fjord),
        granite_time: timestamp(WorldChainHardfork::Granite),
        holocene_time: timestamp(WorldChainHardfork::Holocene),
        isthmus_time: timestamp(WorldChainHardfork::Isthmus),
        jovian_time: timestamp(WorldChainHardfork::Jovian),
        karst_time: timestamp(WorldChainHardfork::Karst),
        tropo_time: timestamp(WorldChainHardfork::Tropo),
        strato_time: timestamp(WorldChainHardfork::Strato),
    }
}

/// Builds an [`OnlineHostConfig`] from either a rollup-config file or a pre-computed hash.
///
/// When `rollup_config_path` is `Some`, the schedule and hash are derived from the file content.
/// When `None`, `rollup_config_hash` must be provided together with the built-in schedule.
pub fn build_online_config(
    rollup_config_path: Option<PathBuf>,
    rollup_config_hash: Option<B256>,
    l1_rpc: String,
    l1_beacon_rpc: String,
    l2_rpc: String,
    l2_chain_id: u64,
    schedule: &WorldRangeHardforkConfig,
    witness_timeout: Duration,
) -> anyhow::Result<OnlineHostConfig> {
    if let Some(path) = rollup_config_path {
        let bytes =
            std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
        let value: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        return OnlineHostConfig::from_rollup_config_value(
            &value,
            l1_rpc,
            l1_beacon_rpc,
            l2_rpc,
            Some(path),
            witness_timeout,
        );
    }

    let rollup_config_hash = rollup_config_hash
        .context("provide --rollup-config or ROLLUP_CONFIG, or supply --rollup-config-hash")?;
    Ok(OnlineHostConfig {
        l1_rpc,
        l1_beacon_rpc,
        l2_rpc,
        schedule: schedule.clone(),
        rollup_config_hash,
        l2_chain_id: Some(l2_chain_id),
        rollup_config_path: None,
        witness_timeout,
    })
}

/// One range-witness request covering L2 blocks `(start_block, end_block]`.
#[derive(Clone, Copy, Debug)]
pub struct RangeWitnessRequest {
    /// Exclusive lower bound: the agreed parent block.
    pub start_block: u64,
    /// Inclusive upper bound: the claimed block.
    pub end_block: u64,
    /// L1 head hash pinning the witness data. When `None`, a finalized L1 block past the
    /// range's L1 origin is resolved automatically.
    pub l1_head: Option<B256>,
    /// Allow proving blocks newer than the finalized L2 head.
    pub allow_unfinalized: bool,
}

/// Witness data plus the human-readable context it was built from.
pub struct RangeProofInput {
    /// Range and chain context captured at build time.
    pub metadata: RangeMetadata,
    /// Witness consumed by the range guest (rkyv-serializable).
    pub witness: WorldRangeWitnessData,
}

/// Context captured while building a range witness.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeMetadata {
    pub start_block: u64,
    pub end_block: u64,
    pub finalized_l2_head: Option<u64>,
    pub l1_head: B256,
    pub l2_pre_root: B256,
    pub l2_post_root: B256,
    pub rollup_config_hash: B256,
    pub active_fork: String,
    pub world_spec_id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RpcBlock {
    number: HexU64,
    hash: B256,
    state_root: B256,
    timestamp: HexU64,
    /// Post-Isthmus this is the `L2ToL1MessagePasser` storage root; pre-Isthmus it is the empty
    /// withdrawals root (Canyon) or absent, and must not be used for the output root.
    #[serde(default)]
    withdrawals_root: Option<B256>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(try_from = "String")]
struct HexU64(u64);

impl TryFrom<String> for HexU64 {
    type Error = anyhow::Error;

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

/// Builds the witness for one L2 block range against live RPC endpoints.
///
/// Synchronous: creates its own Tokio runtime for the Kona host, so it must run on a
/// blocking-capable thread (not inside an async task).
pub async fn build_range_input(
    config: &OnlineHostConfig,
    request: RangeWitnessRequest,
) -> anyhow::Result<RangeProofInput> {
    if request.end_block <= request.start_block {
        bail!(
            "end_block ({}) must be greater than start_block ({})",
            request.end_block,
            request.start_block,
        );
    }

    let client = Client::builder()
        .timeout(RPC_REQUEST_TIMEOUT)
        .build()
        .context("failed to build the witness RPC client")?;

    // Every phase shares one budget so a stuck RPC endpoint cannot extend the job past it.
    let deadline = tokio::time::Instant::now() + config.witness_timeout;
    let inputs = tokio::time::timeout_at(deadline, resolve_host_inputs(&client, config, request))
        .await
        .map_err(|_| {
            anyhow!(
                "resolving Kona host inputs over RPC timed out after {} seconds",
                config.witness_timeout.as_secs()
            )
        })??;
    let HostInputs {
        finalized_l2_head,
        agreed_l2_head_hash,
        claimed_l2_timestamp,
        l1_head,
        pre_root,
        post_root,
    } = inputs;

    let active_fork = config
        .schedule
        .active_fork_at(request.end_block, claimed_l2_timestamp);
    let world_spec_id = WorldRangeSpecId::from_hardfork(active_fork);

    let host = SingleChainHost {
        l1_head,
        agreed_l2_head_hash,
        agreed_l2_output_root: pre_root,
        claimed_l2_output_root: post_root,
        claimed_l2_block_number: request.end_block,
        l2_node_address: Some(trim_rpc_url(&config.l2_rpc)),
        l1_node_address: Some(trim_rpc_url(&config.l1_rpc)),
        l1_beacon_address: Some(trim_rpc_url(&config.l1_beacon_rpc)),
        data_dir: None,
        data_format: DataFormat::default(),
        native: false,
        server: true,
        l2_chain_id: config
            .rollup_config_path
            .is_none()
            .then_some(config.l2_chain_id)
            .flatten(),
        rollup_config_path: config.rollup_config_path.clone(),
        l1_config_path: None,
    };

    let witness = collect_world_range_witness(
        host,
        config.schedule.clone(),
        deadline.saturating_duration_since(tokio::time::Instant::now()),
    )
    .await?;

    Ok(RangeProofInput {
        metadata: RangeMetadata {
            start_block: request.start_block,
            end_block: request.end_block,
            finalized_l2_head,
            l1_head,
            l2_pre_root: pre_root,
            l2_post_root: post_root,
            rollup_config_hash: config.rollup_config_hash,
            active_fork: format!("{active_fork:?}"),
            world_spec_id: <&'static str>::from(world_spec_id).to_string(),
        },
        witness,
    })
}

/// Everything resolved over RPC before the Kona host starts.
struct HostInputs {
    finalized_l2_head: Option<u64>,
    agreed_l2_head_hash: B256,
    claimed_l2_timestamp: u64,
    l1_head: B256,
    pre_root: B256,
    post_root: B256,
}

async fn resolve_host_inputs(
    client: &Client,
    config: &OnlineHostConfig,
    request: RangeWitnessRequest,
) -> anyhow::Result<HostInputs> {
    let finalized_l2_head = finalized_l2_head(client, &config.l2_rpc).await?;
    if !request.allow_unfinalized && finalized_l2_head.is_some_and(|head| request.end_block > head)
    {
        bail!(
            "end_block {} is newer than finalized L2 head {}; witness data may be reorged",
            request.end_block,
            finalized_l2_head.unwrap_or_default(),
        );
    }

    let pre_block = get_block(
        client,
        &config.l2_rpc,
        BlockTag::Number(request.start_block),
    )
    .await?;
    let post_block = get_block(client, &config.l2_rpc, BlockTag::Number(request.end_block)).await?;
    let pre_state = output_root_witness(
        client,
        &config.l2_rpc,
        &config.schedule,
        request.start_block,
        &pre_block,
    )
    .await?;
    let post_state = output_root_witness(
        client,
        &config.l2_rpc,
        &config.schedule,
        request.end_block,
        &post_block,
    )
    .await?;

    let l1_head = match request.l1_head {
        Some(hash) => hash,
        None => resolve_l1_head(client, &config.l2_rpc, &config.l1_rpc, request.end_block).await?,
    };

    Ok(HostInputs {
        finalized_l2_head,
        agreed_l2_head_hash: pre_block.hash,
        claimed_l2_timestamp: post_block.timestamp.0,
        l1_head,
        pre_root: pre_state.output_root(),
        post_root: post_state.output_root(),
    })
}

async fn collect_world_range_witness(
    host: SingleChainHost,
    schedule: WorldRangeHardforkConfig,
    timeout: Duration,
) -> anyhow::Result<WorldRangeWitnessData> {
    let preimage = BidirectionalChannel::new()?;
    let hint = BidirectionalChannel::new()?;
    let mut server_task = host.start_server(hint.host, preimage.host).await?;

    let witness = collect_witness_from_channels(preimage.client, hint.client, schedule);
    tokio::pin!(witness);

    let result = match tokio::time::timeout(timeout, async {
        tokio::select! {
            result = &mut witness => result,
            server_result = &mut server_task => match server_result {
                Ok(Ok(())) => Err(anyhow!("Kona preimage server exited before witness generation completed")),
                Ok(Err(err)) => Err(anyhow!("Kona preimage server failed: {err}")),
                Err(err) => Err(anyhow!("Kona preimage server task panicked: {err}")),
            },
        }
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(anyhow!(
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
) -> anyhow::Result<WorldRangeWitnessData> {
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
    let (boot_info, input, _l2_pre_block_number) = get_inputs_for_pipeline(oracle.clone())
        .await
        .map_err(|err| anyhow!("failed to load Kona pipeline inputs: {err:?}"))?;

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
            .map_err(|err| anyhow!("failed to create Kona derivation pipeline: {err:?}"))?;
        executor
            .run_with_world_schedule(
                boot_info,
                pipeline,
                cursor,
                l2_provider,
                Some(schedule.clone()),
            )
            .await
            .map_err(|err| anyhow!("failed to run Kona derivation/execution: {err:?}"))?;
    }

    let preimages = preimage_witness_store
        .lock()
        .map_err(|_| anyhow!("preimage witness store mutex poisoned"))?
        .clone();
    let blobs = blob_data
        .lock()
        .map_err(|_| anyhow!("blob data mutex poisoned"))?
        .clone();

    Ok(WorldRangeWitnessData::from_parts_with_world_config(
        preimages, blobs, schedule,
    ))
}

/// Fetches the finalized L2 head block number, when the RPC exposes one.
pub async fn finalized_l2_head(client: &Client, rpc_url: &str) -> anyhow::Result<Option<u64>> {
    Ok(rpc::<RpcBlock>(
        client,
        rpc_url,
        "eth_getBlockByNumber",
        json!(["finalized", false]),
    )
    .await?
    .map(|b| b.number.0))
}

async fn get_block(client: &Client, rpc_url: &str, tag: BlockTag) -> anyhow::Result<RpcBlock> {
    rpc(
        client,
        rpc_url,
        "eth_getBlockByNumber",
        json!([tag.to_rpc_value(), false]),
    )
    .await?
    .with_context(|| format!("eth_getBlockByNumber returned null for {}", tag.display()))
}

/// Builds the output-root witness for one L2 block, from the header when Isthmus is active.
///
/// The `eth_getProof` fallback asks the L2 node to rebuild historical state for a block that is
/// typically hundreds behind the head, which is what the header path exists to avoid.
async fn output_root_witness(
    client: &Client,
    rpc_url: &str,
    schedule: &WorldRangeHardforkConfig,
    block_number: u64,
    block: &RpcBlock,
) -> anyhow::Result<OutputRootWitness> {
    if let Some(witness) = output_root_witness_from_header(schedule, block) {
        return Ok(witness);
    }

    let proof = match rpc::<AccountProof>(
        client,
        rpc_url,
        "eth_getProof",
        json!([
            format!("{L2_TO_L1_MESSAGE_PASSER:#x}"),
            Vec::<String>::new(),
            BlockTag::Number(block_number).to_rpc_value(),
        ]),
    )
    .await
    {
        Ok(Some(proof)) => proof,
        Ok(None) => bail!("eth_getProof returned null"),
        Err(proof_err) => {
            return output_root_witness_from_op_node(client, rpc_url, block_number, block)
                .await
                .with_context(|| format!("eth_getProof failed first: {proof_err}"));
        }
    };

    Ok(OutputRootWitness {
        state_root: block.state_root,
        message_passer_storage_root: proof.storage_hash,
        block_hash: block.hash,
    })
}

/// Derives the output-root witness from the block header alone, when that is provably valid.
///
/// Post-Isthmus the header's `withdrawalsRoot` *is* the `L2ToL1MessagePasser` storage root
/// (<https://specs.optimism.io/protocol/isthmus/exec-engine.html#l2tol1messagepasser-storage-root-in-header>),
/// so no proof is needed. Returns `None` — deferring to the proof-based path — for any block where
/// that does not hold: pre-Isthmus `withdrawalsRoot` is the *empty withdrawals* root under Canyon
/// and absent before it, so trusting it there would silently produce a wrong output root.
fn output_root_witness_from_header(
    schedule: &WorldRangeHardforkConfig,
    block: &RpcBlock,
) -> Option<OutputRootWitness> {
    if !schedule.is_active(
        WorldRangeHardfork::Isthmus,
        block.number.0,
        block.timestamp.0,
    ) {
        return None;
    }

    let Some(message_passer_storage_root) = block.withdrawals_root else {
        warn!(
            block_number = block.number.0,
            "post-Isthmus header has no withdrawalsRoot; falling back to historical eth_getProof"
        );
        return None;
    };

    Some(OutputRootWitness {
        state_root: block.state_root,
        message_passer_storage_root,
        block_hash: block.hash,
    })
}

async fn output_root_witness_from_op_node(
    client: &Client,
    rpc_url: &str,
    block_number: u64,
    block: &RpcBlock,
) -> anyhow::Result<OutputRootWitness> {
    let output: OptimismOutputAtBlock = rpc(
        client,
        rpc_url,
        "optimism_outputAtBlock",
        json!([BlockTag::Number(block_number).to_rpc_value()]),
    )
    .await?
    .context("optimism_outputAtBlock returned null")?;

    let witness = OutputRootWitness {
        state_root: output.state_root,
        message_passer_storage_root: output.withdrawal_storage_root,
        block_hash: block.hash,
    };
    let computed = witness.output_root();
    if computed != output.output_root {
        bail!(
            "output root mismatch for block {block_number}: computed {computed}, RPC returned {}",
            output.output_root
        );
    }
    Ok(witness)
}

/// Resolves a finalized L1 head past the range's L1 origin, mirroring the proposer's choice.
pub async fn resolve_l1_head(
    client: &Client,
    l2_rpc: &str,
    l1_rpc: &str,
    end_block: u64,
) -> anyhow::Result<B256> {
    let l1_origin_number = l1_origin_number(client, l2_rpc, end_block).await?;
    let finalized_l1 = get_block(client, l1_rpc, BlockTag::Finalized).await?;
    let target = l1_origin_number
        .saturating_add(20)
        .min(finalized_l1.number.0);
    Ok(get_block(client, l1_rpc, BlockTag::Number(target))
        .await?
        .hash)
}

async fn l1_origin_number(
    client: &Client,
    rpc_url: &str,
    block_number: u64,
) -> anyhow::Result<u64> {
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
    )
    .await?
    .context("eth_call to L1Block.number() returned null")?;

    parse_hex_u64_word(&result)
        .with_context(|| format!("failed to parse L1Block.number() result {result}"))
}

fn parse_hex_u64_word(value: &str) -> anyhow::Result<u64> {
    let value = value
        .strip_prefix("0x")
        .context("hex quantity must start with 0x")?
        .trim_start_matches('0');
    if value.is_empty() {
        return Ok(0);
    }
    Ok(u64::from_str_radix(value, 16)?)
}

/// Fetches an L1 header by hash, e.g. the checkpoint head consumed by the aggregation guest.
pub async fn fetch_l1_header_by_hash(
    client: &Client,
    rpc_url: &str,
    hash: B256,
) -> anyhow::Result<alloy_consensus::Header> {
    let block_json: Value = rpc(client, rpc_url, "eth_getBlockByHash", json!([hash, false]))
        .await?
        .with_context(|| format!("eth_getBlockByHash returned null for {hash:?}"))?;
    serde_json::from_value(block_json).context("failed to deserialize L1 block header")
}

/// Minimal blocking JSON-RPC call helper.
pub async fn rpc<T: DeserializeOwned>(
    client: &Client,
    rpc_url: &str,
    method: &str,
    params: Value,
) -> anyhow::Result<Option<T>> {
    let response: RpcResponse<T> = client
        .post(rpc_url)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        }))
        .send()
        .await
        .with_context(|| format!("failed to call {method}"))?
        .error_for_status()
        .with_context(|| format!("{method} returned HTTP error"))?
        .json()
        .await
        .with_context(|| format!("failed to decode {method} response"))?;

    if let Some(error) = response.error {
        bail!("{method} RPC error {}: {}", error.code, error.message);
    }

    Ok(response.result)
}

fn trim_rpc_url(url: &str) -> String {
    url.trim_end_matches('/').to_string()
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::b256;

    /// World Chain mainnet Isthmus activation, observed as the first header carrying a
    /// message-passer `withdrawalsRoot` (block 22368181); block 22368180 is the last pre-fork one.
    const MAINNET_ISTHMUS_TIME: u64 = 1_764_072_001;

    /// Empty trie root, i.e. the `withdrawalsRoot` every post-Canyon pre-Isthmus header carries.
    const EMPTY_WITHDRAWALS_ROOT: B256 =
        b256!("0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421");

    /// World Chain mainnet block 22368181: first post-Isthmus block.
    fn post_isthmus_block() -> RpcBlock {
        RpcBlock {
            number: HexU64(22_368_181),
            hash: b256!("0x1d4ddb8a89e37696618d768f3e02508b8d6916d191052d419e775a2401302efb"),
            state_root: b256!("0x273a648437dc061c21a87248298be4d9693109e0f06643dc02cbe64a61357089"),
            timestamp: HexU64(1_764_072_001),
            withdrawals_root: Some(b256!(
                "0x689214f9c279bcf419dc24a176286f70505a63c0e1fbe206511ef7c89c5f5aa2"
            )),
        }
    }

    /// World Chain mainnet block 22368180: last pre-Isthmus block, empty `withdrawalsRoot`.
    fn pre_isthmus_block() -> RpcBlock {
        RpcBlock {
            number: HexU64(22_368_180),
            hash: b256!("0xc2198f4973ca7c74035e8f1aafc8634666c6be4b1def1d7ccd77ab871297a9ff"),
            state_root: b256!("0x6f43f8da745ccd8ed511754a625c36764dfec9cbe95c9da0ccb391b3be8192dc"),
            timestamp: HexU64(1_764_071_999),
            withdrawals_root: Some(EMPTY_WITHDRAWALS_ROOT),
        }
    }

    fn schedule(isthmus_time: Option<u64>) -> WorldRangeHardforkConfig {
        WorldRangeHardforkConfig {
            isthmus_time,
            ..Default::default()
        }
    }

    #[test]
    fn derives_witness_from_header_post_isthmus() {
        let block = post_isthmus_block();
        let witness =
            output_root_witness_from_header(&schedule(Some(MAINNET_ISTHMUS_TIME)), &block)
                .expect("post-Isthmus header is sufficient");

        assert_eq!(witness.state_root, block.state_root);
        assert_eq!(witness.block_hash, block.hash);
        assert_eq!(
            Some(witness.message_passer_storage_root),
            block.withdrawals_root
        );
    }

    #[test]
    fn falls_back_on_the_last_pre_isthmus_block() {
        // The header carries an empty `withdrawalsRoot` here, so using it would commit to a wrong
        // output root rather than merely lose the optimization.
        assert!(
            output_root_witness_from_header(
                &schedule(Some(MAINNET_ISTHMUS_TIME)),
                &pre_isthmus_block()
            )
            .is_none()
        );
    }

    #[test]
    fn isthmus_activation_is_inclusive_of_the_activation_timestamp() {
        let block = post_isthmus_block();
        let activation = block.timestamp.0;

        assert!(output_root_witness_from_header(&schedule(Some(activation)), &block).is_some());
        assert!(output_root_witness_from_header(&schedule(Some(activation + 1)), &block).is_none());
    }

    #[test]
    fn falls_back_when_isthmus_is_unscheduled() {
        assert!(output_root_witness_from_header(&schedule(None), &post_isthmus_block()).is_none());
    }

    #[test]
    fn falls_back_when_post_isthmus_header_omits_withdrawals_root() {
        let block = RpcBlock {
            withdrawals_root: None,
            ..post_isthmus_block()
        };

        assert!(
            output_root_witness_from_header(&schedule(Some(MAINNET_ISTHMUS_TIME)), &block)
                .is_none()
        );
    }

    #[test]
    fn deserializes_withdrawals_root_from_an_rpc_header() {
        let with_root: RpcBlock = serde_json::from_value(json!({
            "number": "0x1554fb5",
            "hash": "0x1d4ddb8a89e37696618d768f3e02508b8d6916d191052d419e775a2401302efb",
            "stateRoot": "0x273a648437dc061c21a87248298be4d9693109e0f06643dc02cbe64a61357089",
            "timestamp": "0x69259a41",
            "withdrawalsRoot": "0x689214f9c279bcf419dc24a176286f70505a63c0e1fbe206511ef7c89c5f5aa2",
            "gasUsed": "0x0",
        }))
        .expect("header decodes");
        assert_eq!(with_root.number.0, 22_368_181);
        assert_eq!(with_root.timestamp.0, 1_764_072_001);
        assert_eq!(
            with_root.withdrawals_root,
            post_isthmus_block().withdrawals_root
        );

        let without_root: RpcBlock = serde_json::from_value(json!({
            "number": "0x1",
            "hash": B256::ZERO,
            "stateRoot": B256::ZERO,
            "timestamp": "0x1",
        }))
        .expect("pre-Canyon header decodes");
        assert!(without_root.withdrawals_root.is_none());
    }
}
