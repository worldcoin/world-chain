use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use alloy_primitives::B256;
use anyhow::{Context, Result, bail};
use clap::Args;
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
pub enum Network {
    #[value(name = "worldchain")]
    WorldChain,
    #[value(name = "worldchain-sepolia")]
    WorldChainSepolia,
}

impl Network {
    pub fn chain_id(self) -> u64 {
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

#[derive(Debug, Args)]
pub struct HashRollupConfigArgs {
    /// Rollup config JSON file. Mutually exclusive with --l2-rpc.
    #[arg(long, env = "ROLLUP_CONFIG", conflicts_with = "l2_rpc")]
    pub rollup_config: Option<PathBuf>,

    /// L2 RPC URL to fetch the rollup config from. Mutually exclusive with --rollup-config.
    #[arg(long, env = "L2_RPC_URL", conflicts_with = "rollup_config")]
    pub l2_rpc: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct RpcArgs {
    /// L2 block number to start from (exclusive lower bound; proved range is start+1..=end).
    #[arg(long)]
    pub start_block: u64,

    /// L2 block number to prove up to (inclusive).
    #[arg(long)]
    pub end_block: u64,

    /// World Chain L2 execution RPC URL.
    #[arg(long, env = "L2_RPC_URL")]
    pub l2_rpc: String,

    /// Ethereum L1 execution RPC URL.
    #[arg(long, env = "L1_RPC_URL")]
    pub l1_rpc: String,

    /// Ethereum L1 beacon API URL.
    #[arg(long, env = "L1_BEACON_RPC_URL")]
    pub l1_beacon_rpc: String,

    /// Rollup config JSON file. If omitted, uses the built-in World Chain mainnet config.
    #[arg(long, env = "ROLLUP_CONFIG")]
    pub rollup_config: Option<PathBuf>,

    /// Rollup config hash override (required when --rollup-config is not supplied).
    #[arg(long, env = "ROLLUP_CONFIG_HASH")]
    pub rollup_config_hash: Option<B256>,

    /// L1 head hash override. Defaults to a finalized L1 block after the L2 range.
    #[arg(long, env = "L1_HEAD")]
    pub l1_head: Option<B256>,

    /// Allow proving blocks newer than the finalized L2 head.
    #[arg(long)]
    pub allow_unfinalized: bool,

    /// Maximum seconds to spend generating the Kona witness.
    #[arg(long, default_value_t = 900)]
    pub witness_timeout_seconds: u64,

    /// World Chain network to prove.
    #[arg(long, env = "NETWORK", default_value = "worldchain")]
    pub network: Network,
}

#[derive(Debug, Args)]
pub struct WitnessArgs {
    #[command(flatten)]
    pub rpc: RpcArgs,

    /// Output path for the rkyv-serialized witness bytes.
    #[arg(long)]
    pub output: PathBuf,
}

pub fn print_rollup_config_hash(args: HashRollupConfigArgs) -> Result<()> {
    let hash = rollup_config_hash_from_args(args)?;
    println!("{hash:?}");
    Ok(())
}

pub fn rollup_config_hash_from_args(args: HashRollupConfigArgs) -> Result<B256> {
    match (args.rollup_config, args.l2_rpc) {
        (Some(path), _) => Ok(proof_config_from_file(&path)?.1),
        (None, Some(url)) => {
            let client = Client::new();
            let value: Value = rpc(&client, &url, "optimism_rollupConfig", json!([]))?
                .context("optimism_rollupConfig returned null")?;
            Ok(world_chain_proof_protocol::hash_rollup_config(&value)?)
        }
        (None, None) => bail!("provide --rollup-config or --l2-rpc"),
    }
}

pub fn write_witness(args: WitnessArgs) -> Result<()> {
    let input = build_range_input_from_args(&args.rpc)?;
    let bytes = witness_bytes(&input.witness)?;
    write_bytes(&args.output, &bytes)?;
    let metadata_path = sibling_path(&args.output, "metadata.json");
    write_json(&metadata_path, &json!({ "metadata": input.metadata }))?;
    println!("witness bytes: {}", args.output.display());
    println!("metadata:      {}", metadata_path.display());
    Ok(())
}

/// Resolves the online host config (RPC endpoints + proof config) from CLI args.
pub fn online_host_config(args: &RpcArgs) -> Result<OnlineHostConfig> {
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

pub fn build_range_input_from_args(args: &RpcArgs) -> Result<RangeProofInput> {
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

pub fn proof_config(
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

pub fn proof_config_from_file(path: &Path) -> Result<(WorldRangeHardforkConfig, B256)> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let protocol_config = ProtocolHardforkConfig::from_rollup_config_value(&value)?;
    let hash = world_chain_proof_protocol::hash_rollup_config(&value)?;
    Ok((range_hardfork_config(&protocol_config), hash))
}

pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    ensure_parent_dir(path)?;
    fs::write(path, serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("failed to write {}", path.display()))
}

pub fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    Ok(())
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
        karst_time: config.karst_time,
        tropo_time: config.tropo_time,
        strato_time: config.strato_time,
    }
}

fn witness_bytes(witness: &WorldRangeWitnessData) -> Result<Vec<u8>> {
    Ok(rkyv::to_bytes::<rkyv::rancor::Error>(witness)?.to_vec())
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
