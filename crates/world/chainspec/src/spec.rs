use reth_cli::chainspec::ChainSpecParser;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_cli::chainspec::OpChainSpecParser;
use std::sync::Arc;

use crate::alchemy_sepolia::ALCHEMY_SEPOLIA;

const WORLD_CHAIN_SUPPORTED_CHAINS: &[&str] = &["alchemy-sepolia"];

/// Optimism chain specification parser.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct WorldChainChainSpecParser;

impl ChainSpecParser for WorldChainChainSpecParser {
    type ChainSpec = OpChainSpec;

    const SUPPORTED_CHAINS: &[&str] = WORLD_CHAIN_SUPPORTED_CHAINS;

    fn parse(s: &str) -> eyre::Result<Arc<Self::ChainSpec>> {
        chain_value_parser(s)
    }
}

/// Clap value parser for [`OpChainSpec`]s.
///
/// The value parser matches either a known chain, the path
/// to a json file, or a json formatted string in-memory. The json needs to be a Genesis struct.
pub fn chain_value_parser(s: &str) -> eyre::Result<Arc<OpChainSpec>> {
    if s == "alchemy-sepolia" {
        Ok(ALCHEMY_SEPOLIA.clone())
    } else {
        OpChainSpecParser::parse(s)
    }
}
