use std::sync::Arc;

use reth_cli::chainspec::{ChainSpecParser, parse_genesis};
use reth_optimism_chainspec::SUPPORTED_CHAINS;
use world_chain_chainspec::WorldChainSpec;
use world_chain_manifest::NetworkManifest;

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
/// Matches, in order: a path to a `.toml` network manifest (which derives the
/// chain spec from a predefined base spec and the committed hardfork schedule),
/// a known OP stack chain, a path to a genesis JSON file, or an in-memory
/// genesis JSON string.
pub fn chain_value_parser(s: &str) -> eyre::Result<Arc<WorldChainSpec>> {
    if NetworkManifest::is_manifest_path(s) {
        let manifest = NetworkManifest::load(s)?;
        return Ok(Arc::new(manifest.into_chain_spec()?));
    }
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
