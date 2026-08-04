use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use alloy_primitives::B256;
use anyhow::{Context, Result, bail};
use clap::Args;
use reqwest::Client;
use serde::Serialize;
use serde_json::{Value, json};
use world_chain_chainspec::WorldChainSpec;
use world_chain_proof_core::{
    hash_world_rollup_config, range::WorldRangeHardforkConfig, witness::WorldRangeWitnessData,
};
use world_chain_proof_kona_host_utils::online::{
    OnlineHostConfig, RangeProofInput, RangeWitnessRequest, build_range_input,
    hardfork_config_from_chain_spec, rpc,
};

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

    /// L2 consensus RPC URL to fetch the rollup config from. Mutually exclusive with --rollup-config.
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

    /// op-node RPC serving `optimism_outputAtBlock`, used only as the `eth_getProof` fallback.
    /// Must not be the execution RPC.
    #[arg(long, env = "L2_CONSENSUS_RPC_URL")]
    pub l2_consensus_rpc: Option<String>,

    /// Ethereum L1 execution RPC URL.
    #[arg(long, env = "L1_RPC_URL")]
    pub l1_rpc: String,

    /// Ethereum L1 beacon API URL.
    #[arg(long, env = "L1_BEACON_RPC_URL")]
    pub l1_beacon_rpc: String,

    /// Rollup config JSON file. If omitted, uses the selected network's built-in fork schedule
    /// and requires --rollup-config-hash.
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

pub async fn print_rollup_config_hash(args: HashRollupConfigArgs) -> Result<()> {
    let hash = rollup_config_hash_from_args(args).await?;
    println!("{hash:?}");
    Ok(())
}

pub async fn rollup_config_hash_from_args(args: HashRollupConfigArgs) -> Result<B256> {
    match (args.rollup_config, args.l2_rpc) {
        (Some(path), _) => Ok(proof_config_from_file(&path)?.1),
        (None, Some(url)) => {
            let client = Client::new();
            let value: Value = rpc(&client, &url, "optimism_rollupConfig", json!([]))
                .await?
                .context("optimism_rollupConfig returned null")?;
            Ok(rollup_config_hash_from_value(&value)?.1)
        }
        (None, None) => bail!("provide --rollup-config or --l2-rpc"),
    }
}

pub async fn write_witness(args: WitnessArgs) -> Result<()> {
    let input = build_range_input_from_args(&args.rpc).await?;
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
        l2_consensus_rpc: args.l2_consensus_rpc.clone(),
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

pub async fn build_range_input_from_args(args: &RpcArgs) -> Result<RangeProofInput> {
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
    .await
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
    Ok((hardfork_config_from_chain_spec(spec.as_ref()), hash))
}

pub fn proof_config_from_file(path: &Path) -> Result<(WorldRangeHardforkConfig, B256)> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    rollup_config_hash_from_value(&value)
}

/// Parses a rollup config JSON `Value` into its World fork schedule and rollup config hash.
///
/// The hash MUST be computed from the same parsed [`kona_genesis::RollupConfig`] the
/// enclave/guest hashes via [`hash_world_rollup_config`], not from the raw JSON text (via
/// `hash_rollup_config`). Otherwise `just proof-rollup-config-hash` (used to derive the
/// on-chain `ROLLUP_CONFIG_HASH`) and this CLI's prover path would keep producing a hash that
/// cannot match the worker/enclave for any `rollup.json` that isn't already in Kona's exact
/// canonical serialized form (see `OnlineHostConfig::from_rollup_config_value`).
fn rollup_config_hash_from_value(value: &Value) -> Result<(WorldRangeHardforkConfig, B256)> {
    let schedule: WorldRangeHardforkConfig =
        serde_json::from_value(value.clone()).context("failed to parse rollup config hardforks")?;
    let parsed_rollup_config: kona_genesis::RollupConfig =
        serde_json::from_value(value.clone()).context("failed to parse rollup config")?;
    let hash = hash_world_rollup_config(&parsed_rollup_config, &schedule)
        .context("failed to hash rollup config")?;
    Ok((schedule, hash))
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

#[cfg(test)]
mod tests {
    use super::*;
    use world_chain_proof_core::hash_rollup_config;

    /// Regression test ensuring `proof_config_from_file` (and therefore
    /// `rollup_config_hash_from_args`, used by `just proof-rollup-config-hash` for the on-chain
    /// `ROLLUP_CONFIG_HASH` and by the CLI prover path) hashes the parsed [`kona_genesis::RollupConfig`]
    /// via [`hash_world_rollup_config`], not the raw source JSON via `hash_rollup_config`.
    ///
    /// Mirrors `world_chain_proof_core::boot`'s regression test for the same bug class: a
    /// rollup.json missing `granite_channel_timeout` (e.g. one produced by `op-node` instead of
    /// `kona-node`) must not silently produce a hash that diverges from what the enclave/guest
    /// actually commits to via [`hash_world_rollup_config`].
    #[test]
    fn proof_config_from_file_hashes_parsed_config_not_raw_json() {
        let raw = serde_json::json!({
            "genesis": {
                "l1": { "hash": format!("0x{}", "11".repeat(32)), "number": 1 },
                "l2": { "hash": format!("0x{}", "22".repeat(32)), "number": 0 },
                "l2_time": 0,
                "system_config": null,
            },
            "block_time": 2,
            "max_sequencer_drift": 600,
            "seq_window_size": 3600,
            "channel_timeout": 300,
            "l1_chain_id": 11155111,
            "l2_chain_id": 5496749,
            "regolith_time": 0,
            "canyon_time": 0,
            "delta_time": 0,
            "ecotone_time": 0,
            "fjord_time": 0,
            "granite_time": 0,
            "holocene_time": 0,
            "isthmus_time": 0,
            "batch_inbox_address": format!("0x{}", "33".repeat(20)),
            "deposit_contract_address": format!("0x{}", "44".repeat(20)),
            "l1_system_config_address": format!("0x{}", "55".repeat(20)),
        });

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollup.json");
        fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();

        let (_, hash) = proof_config_from_file(&path).unwrap();

        // The old (buggy) behavior hashed the raw JSON text directly.
        let raw_hash = hash_rollup_config(&raw).unwrap();
        assert_ne!(
            hash, raw_hash,
            "proof_config_from_file must not hash the raw source JSON — it must hash the \
             parsed RollupConfig (via hash_world_rollup_config), matching the enclave/guest. \
             This diverges here because granite_channel_timeout is absent from the source JSON \
             but is filled in with a default when kona_genesis::RollupConfig re-serializes it."
        );

        // The new (correct) behavior must match hashing the parsed RollupConfig directly.
        let schedule: WorldRangeHardforkConfig = serde_json::from_value(raw.clone()).unwrap();
        let parsed: kona_genesis::RollupConfig = serde_json::from_value(raw).unwrap();
        let expected = hash_world_rollup_config(&parsed, &schedule).unwrap();
        assert_eq!(hash, expected);
    }
}
