use reth_chainspec::ChainSpec;
use reth_primitives::Genesis as GenesisBlock;
use std::path::Path;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct Rollup {
    pub genesis: Genesis,
    pub block_time: u64,
    pub max_sequencer_drift: u64,
    pub seq_window_size: u64,
    pub channel_timeout: u64,
    pub l1_chain_id: u64,
    pub l2_chain_id: u64,
    pub regolith_time: u64,
    pub canyon_time: u64,
    pub delta_time: u64,
    pub ecotone_time: u64,
    pub fjord_time: u64,
    pub batch_inbox_address: String,
    pub deposit_contract_address: String,
    pub l1_system_config_address: String,
    pub protocol_versions_address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub da_challenge_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub da_resolve_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_plasma: Option<bool>,
}

#[derive(Deserialize, Serialize)]
pub struct Genesis {
    pub l1: BlockAttributes,
    pub l2: BlockAttributes,
    pub l2_time: u64,
    pub system_config: SystemConfig,
}

#[derive(Deserialize, Serialize)]
pub struct BlockAttributes {
    pub hash: String,
    pub number: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemConfig {
    pub batcher_addr: String,
    pub overhead: String,
    pub scalar: String,
    pub gas_limit: u64,
}

/// Reads in genesis l1, and genesis l2 configuration, and updates the rollup.json file according to spec.
fn main() {
    let genesis_l1: GenesisBlock = serde_json::from_str(
        std::fs::read_to_string(Path::new("/static/genesis/genesis-l1.json"))
            .unwrap()
            .as_str(),
    )
    .unwrap();
    let genesis_l2: GenesisBlock = serde_json::from_str(
        std::fs::read_to_string(Path::new("/static/genesis/genesis-l2.json"))
            .unwrap()
            .as_str(),
    )
    .unwrap();

    let chain_spec_l1 = ChainSpec::from(genesis_l1);
    let chain_spec_l2 = ChainSpec::from(genesis_l2);

    let mut rollup: Rollup = serde_json::from_str(
        std::fs::read_to_string(Path::new("/static/genesis/rollup.json"))
            .unwrap()
            .as_str(),
    )
    .unwrap();

    // Update the rollup.json file with the new genesis hashes.
    rollup.genesis.l1.hash = chain_spec_l1.genesis_hash().to_string();
    rollup.genesis.l2.hash = chain_spec_l2.genesis_hash().to_string();

    // Write the updated rollup.json file.
    std::fs::write(
        "/static/genesis/rollup.json",
        serde_json::to_string_pretty(&rollup).unwrap(),
    )
    .unwrap();
}