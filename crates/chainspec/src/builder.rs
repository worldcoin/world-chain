use alloy_chains::Chain;
use alloy_genesis::Genesis;
use alloy_hardforks::Hardfork;
use alloy_primitives::U256;
use reth_chainspec::{ChainHardforks, ChainSpecBuilder, EthereumHardfork, ForkCondition};
use reth_optimism_chainspec::make_op_genesis_header;
use reth_primitives_traits::SealedHeader;

use crate::{
    Wip1001ActivationConfig, WorldChainHardfork, WorldChainSpec,
    tropo_wip1001_parameters_for_chain,
    wip1001::{Wip1001ActivationConfigError, Wip1001ActivationReadinessError},
};

/// Chain spec builder for a World Chain stack chain.
#[derive(Debug, Default)]
pub struct WorldChainSpecBuilder {
    /// Inner reth chain spec builder.
    inner: ChainSpecBuilder,
    /// Optional WIP-1001 activation parameter set; required when Tropo is scheduled.
    tropo_wip1001_parameters: Option<Wip1001ActivationConfig>,
}

impl WorldChainSpecBuilder {
    /// Construct a new builder from the World Chain mainnet chain spec.
    pub fn mainnet() -> Self {
        let spec = WorldChainSpec::mainnet();
        let mut inner = ChainSpecBuilder::default()
            .chain(spec.chain)
            .genesis(spec.genesis.clone());
        inner = inner.with_forks(spec.hardforks.clone());

        Self {
            inner,
            tropo_wip1001_parameters: None,
        }
    }

    /// Construct a new builder from the World Chain Sepolia chain spec.
    pub fn sepolia() -> Self {
        let spec = WorldChainSpec::sepolia();
        let mut inner = ChainSpecBuilder::default()
            .chain(spec.chain)
            .genesis(spec.genesis.clone());
        inner = inner.with_forks(spec.hardforks.clone());

        Self {
            inner,
            tropo_wip1001_parameters: None,
        }
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
        self.inner = self.inner.with_fork(fork, condition);
        self
    }

    /// Add the given forks with the given activation condition to the spec.
    pub fn with_forks(mut self, forks: ChainHardforks) -> Self {
        self.inner = self.inner.with_forks(forks);
        self
    }

    /// Remove the given fork from the spec.
    pub fn without_fork(mut self, fork: WorldChainHardfork) -> Self {
        self.inner = self.inner.without_fork(fork);
        self
    }

    /// Set validated WIP-1001 activation parameters.
    pub fn with_tropo_wip1001_parameters(
        mut self,
        config: Wip1001ActivationConfig,
    ) -> Result<Self, Wip1001ActivationConfigError> {
        config.validate()?;
        self.tropo_wip1001_parameters = Some(config);
        Ok(self)
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

    /// Enable Tropo at genesis.
    pub fn tropo_activated(mut self) -> Self {
        self = self.jovian_activated();
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
        inner.genesis_header =
            SealedHeader::seal_slow(make_op_genesis_header(&inner.genesis, &inner.hardforks));
        inner
            .paris_block_and_final_difficulty
            .get_or_insert((0, U256::ZERO));

        let tropo_wip1001_parameters = self
            .tropo_wip1001_parameters
            .or_else(|| tropo_wip1001_parameters_for_chain(inner.chain));

        if let Some(config) = tropo_wip1001_parameters {
            config
                .validate()
                .expect("WIP-1001 activation config must be internally valid");
        }

        WorldChainSpec {
            inner,
            tropo_wip1001_parameters,
        }
    }

    /// Build and verify the resulting spec is ready for the configured Tropo schedule.
    ///
    /// Use this for launch paths where a scheduled Tropo fork must not run without complete
    /// WIP-1001 activation parameters. Tests that intentionally construct incomplete specs can use
    /// [`Self::build`] and call [`WorldChainSpec::validate_wip1001_activation_readiness`] directly.
    pub fn try_build(self) -> Result<WorldChainSpec, Wip1001ActivationReadinessError> {
        let spec = self.build();
        spec.validate_wip1001_activation_readiness()?;
        Ok(spec)
    }
}
