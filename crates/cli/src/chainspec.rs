use std::sync::Arc;

use reth_cli::chainspec::{ChainSpecParser, parse_genesis};
use reth_optimism_chainspec::SUPPORTED_CHAINS;
use world_chain_chainspec::WorldChainSpec;

/// World Chain chain specification parser.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct WorldChainSpecParser;

impl ChainSpecParser for WorldChainSpecParser {
    type ChainSpec = WorldChainSpec;

    const SUPPORTED_CHAINS: &'static [&'static str] = SUPPORTED_CHAINS;

    fn parse(s: &str) -> eyre::Result<Arc<Self::ChainSpec>> {
        chain_value_parser(s)
    }
}

/// Clap value parser for [`WorldChainSpec`]s.
///
/// Matches either a known OP stack chain, a path to a genesis JSON file, or an in-memory genesis
/// JSON string.
pub fn chain_value_parser(s: &str) -> eyre::Result<Arc<WorldChainSpec>> {
    if let Some(world_chain_spec) = WorldChainSpec::parse_chain(s) {
        Ok(world_chain_spec)
    } else {
        Ok(Arc::new(parse_genesis(s)?.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_chain_spec() {
        for &chain in WorldChainSpecParser::SUPPORTED_CHAINS {
            assert!(
                <WorldChainSpecParser as ChainSpecParser>::parse(chain).is_ok(),
                "Failed to parse {chain}"
            );
        }
    }
}
