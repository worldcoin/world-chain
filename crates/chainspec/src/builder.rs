use alloy_chains::Chain;
use alloy_genesis::Genesis;
use alloy_hardforks::Hardfork;
use alloy_primitives::U256;
use derive_more::From;
use reth_chainspec::{ChainHardforks, ChainSpecBuilder, EthereumHardfork, ForkCondition};
use reth_optimism_chainspec::make_op_genesis_header;
use reth_primitives_traits::SealedHeader;

use crate::{
    WorldChainHardfork, WorldChainSpec,
    spec::{convert_op_hardfork, convert_op_hardforks},
};

/// Chain spec builder for a World Chain stack chain.
#[derive(Debug, Default, From)]
pub struct WorldChainSpecBuilder {
    /// Inner reth chain spec builder.
    inner: ChainSpecBuilder,
}

impl WorldChainSpecBuilder {
    /// Construct a new builder from the World Chain mainnet chain spec.
    pub fn mainnet() -> Self {
        let spec = WorldChainSpec::mainnet();
        let mut inner = ChainSpecBuilder::default()
            .chain(spec.chain)
            .genesis(spec.genesis.clone());
        inner = inner.with_forks(spec.hardforks.clone());

        Self { inner }
    }

    /// Construct a new builder from the World Chain Sepolia chain spec.
    pub fn sepolia() -> Self {
        let spec = WorldChainSpec::sepolia();
        let mut inner = ChainSpecBuilder::default()
            .chain(spec.chain)
            .genesis(spec.genesis.clone());
        inner = inner.with_forks(spec.hardforks.clone());

        Self { inner }
    }

    /// Set the chain ID.
    pub fn chain(mut self, chain: Chain) -> Self {
        self.inner = self.inner.chain(chain);
        self
    }

    /// Set the genesis block.
    pub fn genesis(mut self, genesis: Genesis) -> Self {
        self.inner = self.inner.genesis(genesis);
        self
    }

    /// Add the given fork with the given activation condition to the spec.
    pub fn with_fork<H: Hardfork>(mut self, fork: H, condition: ForkCondition) -> Self {
        let Some(fork) = convert_op_hardfork(&fork) else {
            return self;
        };
        self.inner = self.inner.with_fork(fork, condition);
        self
    }

    /// Add the given forks with the given activation condition to the spec.
    pub fn with_forks(mut self, forks: ChainHardforks) -> Self {
        let forks = convert_op_hardforks(&forks);
        self.inner = self.inner.with_forks(forks);
        self
    }

    /// Remove the given fork from the spec.
    pub fn without_fork(mut self, fork: WorldChainHardfork) -> Self {
        self.inner = self.inner.without_fork(fork);
        self
    }

    /// Enable Bedrock at genesis.
    pub fn bedrock_activated(mut self) -> Self {
        self.inner = self.inner.paris_activated();
        self.inner = self
            .inner
            .with_fork(WorldChainHardfork::Bedrock, ForkCondition::Block(0));
        self
    }

    /// Enable Regolith at genesis.
    pub fn regolith_activated(mut self) -> Self {
        self = self.bedrock_activated();
        self.inner = self
            .inner
            .with_fork(WorldChainHardfork::Regolith, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Canyon at genesis.
    pub fn canyon_activated(mut self) -> Self {
        self = self.regolith_activated();
        self.inner = self
            .inner
            .with_fork(EthereumHardfork::Shanghai, ForkCondition::Timestamp(0));
        self.inner = self
            .inner
            .with_fork(WorldChainHardfork::Canyon, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Ecotone at genesis.
    pub fn ecotone_activated(mut self) -> Self {
        self = self.canyon_activated();
        self.inner = self
            .inner
            .with_fork(EthereumHardfork::Cancun, ForkCondition::Timestamp(0));
        self.inner = self
            .inner
            .with_fork(WorldChainHardfork::Ecotone, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Fjord at genesis.
    pub fn fjord_activated(mut self) -> Self {
        self = self.ecotone_activated();
        self.inner = self
            .inner
            .with_fork(WorldChainHardfork::Fjord, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Granite at genesis.
    pub fn granite_activated(mut self) -> Self {
        self = self.fjord_activated();
        self.inner = self
            .inner
            .with_fork(WorldChainHardfork::Granite, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Holocene at genesis.
    pub fn holocene_activated(mut self) -> Self {
        self = self.granite_activated();
        self.inner = self
            .inner
            .with_fork(WorldChainHardfork::Holocene, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Isthmus at genesis.
    pub fn isthmus_activated(mut self) -> Self {
        self = self.holocene_activated();
        self.inner = self
            .inner
            .with_fork(EthereumHardfork::Prague, ForkCondition::Timestamp(0));
        self.inner = self
            .inner
            .with_fork(WorldChainHardfork::Isthmus, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Jovian at genesis.
    pub fn jovian_activated(mut self) -> Self {
        self = self.isthmus_activated();
        self.inner = self
            .inner
            .with_fork(WorldChainHardfork::Jovian, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Karst at genesis.
    pub fn karst_activated(mut self) -> Self {
        self = self.jovian_activated();
        self.inner = self
            .inner
            .with_fork(EthereumHardfork::Osaka, ForkCondition::Timestamp(0));
        self.inner = self
            .inner
            .with_fork(WorldChainHardfork::Karst, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Tropo at genesis.
    pub fn tropo_activated(mut self) -> Self {
        self = self.karst_activated();
        self.inner = self
            .inner
            .with_fork(WorldChainHardfork::Tropo, ForkCondition::Timestamp(0));
        self
    }

    /// Enable Strato at genesis.
    pub fn strato_activated(mut self) -> Self {
        self = self.tropo_activated();
        self.inner = self
            .inner
            .with_fork(WorldChainHardfork::Strato, ForkCondition::Timestamp(0));
        self
    }

    /// Build the resulting [`WorldChainSpec`].
    ///
    /// # Panics
    ///
    /// Panics if chain ID or genesis is not set.
    pub fn build(self) -> WorldChainSpec {
        let mut inner = self.inner.build();
        inner.hardforks = convert_op_hardforks(&inner.hardforks);
        inner.genesis_header =
            SealedHeader::seal_slow(make_op_genesis_header(&inner.genesis, &inner.hardforks));
        inner
            .paris_block_and_final_difficulty
            .get_or_insert((0, U256::ZERO));

        WorldChainSpec { inner }
    }
}
