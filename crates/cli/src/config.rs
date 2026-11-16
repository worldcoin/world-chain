use reth_optimism_payload_builder::config::OpBuilderConfig;
use std::path::PathBuf;

use crate::cli::WorldChainArgs;

/// Configuration for the flashblocks payload builder.
#[derive(Default, Debug, Clone)]
pub struct FlashblocksPayloadBuilderConfig {
    /// Inner Optimism payload builder configuration.
    pub inner: OpBuilderConfig,

    /// Whether to enable Block Access List (BAL) generation.
    pub bal_enabled: bool,
}

/// Configuration for optional flashblocks persistence.
///
/// When configured, accepted ordered flashblocks are written to a standalone
/// libmdbx database outside the main node database.
#[derive(Debug, Clone)]
pub struct FlashblocksStoreConfig {
    /// libmdbx database directory used by the flashblocks recorder.
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct WorldChainNodeConfig {
    /// Validated PBH signing backend, absent on nodes without signing credentials.
    pub pbh_wallet: Option<world_chain_signer::PbhWallet>,
    /// Remote publisher credentials and expected identity, validated at startup.
    pub publication_signer: Option<world_chain_signer::PublicationSigner>,
    /// World Chain Specific CLI arguments
    pub args: WorldChainArgs,
    /// World Chain Payload Builder Configuration
    pub builder_config: FlashblocksPayloadBuilderConfig,
    /// Optional flashblocks recorder configuration.
    pub flashblocks_store: Option<FlashblocksStoreConfig>,
}
