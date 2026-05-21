use std::{boxed::Box, sync::Arc, vec, vec::Vec};

use alloy_chains::{Chain, NamedChain};
use alloy_consensus::{BlockHeader, Header};
use alloy_eips::eip7840::BlobParams;
use alloy_genesis::Genesis;
use alloy_hardforks::Hardfork;
use alloy_primitives::{B256, U256};
use derive_more::Deref;
use reth_chainspec::{
    BaseFeeParams, BaseFeeParamsKind, ChainHardforks, ChainSpec, DepositContract, DisplayHardforks,
    EthChainSpec, EthereumHardfork, EthereumHardforks, ForkCondition, ForkFilter, ForkId,
    Hardforks, Head,
};
use reth_network_peers::NodeRecord;
use reth_optimism_chainspec::{
    OpChainSpec, compute_jovian_base_fee, decode_holocene_base_fee, generated_chain_value_parser,
    make_op_genesis_header,
};
use reth_optimism_forks::{OP_MAINNET_HARDFORKS, OpHardfork, OpHardforks};
use reth_primitives_traits::SealedHeader;

use crate::{
    Wip1001ActivationConfig, WorldChainHardfork, WorldChainHardforks,
    strato_wip1001_parameters_for_chain,
    wip1001::{Wip1001ActivationConfigError, Wip1001ActivationReadinessError},
};

/// World Chain Jovian activation timestamp on Sepolia.
pub const JOVIAN_UPGRADE_TIMESTAMP_SEPOLIA: u64 = 1_777_161_600;

/// World Chain Jovian activation timestamp on mainnet.
pub const JOVIAN_UPGRADE_TIMESTAMP_MAINNET: u64 = 1_777_593_600;

/// World Chain Strato activation timestamp on Sepolia.
///
/// Keep this unset until the final production activation timestamp is available.
pub const STRATO_UPGRADE_TIMESTAMP_SEPOLIA: Option<u64> = None;

/// World Chain Strato activation timestamp on mainnet.
///
/// Keep this unset until the final production activation timestamp is available.
pub const STRATO_UPGRADE_TIMESTAMP_MAINNET: Option<u64> = None;

/// World Chain spec type.
///
/// This wraps reth's generic [`ChainSpec`] the same way the OP stack spec does, while using World
/// Chain hardfork names as the canonical post-Jovian schedule.
#[derive(Debug, Clone, Deref, PartialEq, Eq)]
pub struct WorldChainSpec {
    /// Inner reth chain spec.
    #[deref]
    pub inner: ChainSpec,
    /// WIP-1001 parameters that activate with Strato.
    pub(crate) strato_wip1001_parameters: Option<Wip1001ActivationConfig>,
}

impl WorldChainSpec {
    /// Wraps a reth chain spec as a World Chain spec.
    pub fn new(inner: ChainSpec) -> Self {
        Self::from(inner)
    }

    /// Converts the given [`Genesis`] into a [`WorldChainSpec`].
    pub fn from_genesis(genesis: Genesis) -> Self {
        genesis.into()
    }

    /// Parses a built-in OP stack chain spec and wraps it as a World Chain spec.
    pub fn parse_chain(s: &str) -> Option<Arc<Self>> {
        generated_chain_value_parser(s).map(|spec| Arc::new(Self::from((*spec).clone())))
    }

    /// Returns the built-in World Chain mainnet spec.
    pub fn mainnet() -> Arc<Self> {
        Self::parse_chain("worldchain").expect("worldchain is a supported OP stack chain")
    }

    /// Returns the built-in World Chain Sepolia spec.
    pub fn sepolia() -> Arc<Self> {
        Self::parse_chain("worldchain-sepolia")
            .expect("worldchain-sepolia is a supported OP stack chain")
    }

    /// Returns the built-in OP dev spec wrapped as a World Chain spec.
    pub fn dev() -> Arc<Self> {
        Self::parse_chain("dev").expect("dev is a supported OP stack chain")
    }

    /// Adds or replaces a hardfork activation and recomputes the genesis header.
    pub fn set_fork<H: Hardfork>(&mut self, fork: H, condition: ForkCondition) {
        self.inner.hardforks.insert(fork, condition);
        self.inner.genesis_header = SealedHeader::seal_slow(make_op_genesis_header(
            &self.inner.genesis,
            &self.inner.hardforks,
        ));
    }

    /// Set validated WIP-1001 activation parameters.
    ///
    /// The parameters do not activate WIP-1001 by themselves; Strato must also be active at the
    /// evaluated timestamp.
    pub fn set_strato_wip1001_parameters(
        &mut self,
        config: Wip1001ActivationConfig,
    ) -> Result<(), Wip1001ActivationConfigError> {
        config.validate()?;
        self.strato_wip1001_parameters = Some(config);
        Ok(())
    }

    /// Returns Strato/WIP-1001 parameters iff Strato is active and parameters are assigned.
    pub fn strato_wip1001_parameters_at_timestamp(
        &self,
        timestamp: u64,
    ) -> Option<&Wip1001ActivationConfig> {
        self.is_strato_active_at_timestamp(timestamp)
            .then_some(self.strato_wip1001_parameters.as_ref())
            .flatten()
    }

    /// Verifies that the chain spec is safe to run with the configured Strato fork schedule.
    pub fn validate_wip1001_activation_readiness(
        &self,
    ) -> Result<(), Wip1001ActivationReadinessError> {
        let strato_scheduled = !matches!(
            self.world_chain_fork_activation(WorldChainHardfork::Strato),
            ForkCondition::Never
        );
        let is_world_production_chain = matches!(
            self.chain().named(),
            Some(NamedChain::World | NamedChain::WorldSepolia)
        );

        if is_world_production_chain && let Some(config) = self.strato_wip1001_parameters {
            if Some(config) != strato_wip1001_parameters_for_chain(self.chain()) {
                return Err(Wip1001ActivationReadinessError::ProductionConfigMismatch);
            }
        }

        if strato_scheduled {
            let Some(config) = self.strato_wip1001_parameters.as_ref() else {
                return Err(Wip1001ActivationReadinessError::StratoScheduledWithoutConfig);
            };
            config
                .validate()
                .map_err(|_| Wip1001ActivationReadinessError::InvalidActivationConfig)?;
        }

        Ok(())
    }

    /// Applies World Chain defaults that are not yet represented in the upstream OP stack chain
    /// specs. Only fills in unset defaults; operator-supplied fork timestamps and WIP-1001
    /// parameter sets are preserved.
    pub fn apply_world_chain_defaults(&mut self) {
        self.apply_world_chain_hardfork_defaults();
        self.apply_strato_wip1001_parameters_defaults();
    }

    fn apply_world_chain_hardfork_defaults(&mut self) {
        match self.chain().named() {
            Some(NamedChain::World) => {
                self.set_world_chain_hardfork_timestamp_default(
                    WorldChainHardfork::Jovian,
                    Some(JOVIAN_UPGRADE_TIMESTAMP_MAINNET),
                );
                self.set_world_chain_hardfork_timestamp_default(
                    WorldChainHardfork::Strato,
                    STRATO_UPGRADE_TIMESTAMP_MAINNET,
                );
            }
            Some(NamedChain::WorldSepolia) => {
                self.set_world_chain_hardfork_timestamp_default(
                    WorldChainHardfork::Jovian,
                    Some(JOVIAN_UPGRADE_TIMESTAMP_SEPOLIA),
                );
                self.set_world_chain_hardfork_timestamp_default(
                    WorldChainHardfork::Strato,
                    STRATO_UPGRADE_TIMESTAMP_SEPOLIA,
                );
            }
            _ => {}
        }
    }

    fn set_world_chain_hardfork_timestamp_default(
        &mut self,
        fork: WorldChainHardfork,
        timestamp: Option<u64>,
    ) {
        if matches!(self.inner.fork(fork), ForkCondition::Never)
            && let Some(timestamp) = timestamp
        {
            self.set_fork(fork, ForkCondition::Timestamp(timestamp));
        }
    }

    fn apply_strato_wip1001_parameters_defaults(&mut self) {
        self.apply_strato_wip1001_parameters_default(strato_wip1001_parameters_for_chain(
            self.chain(),
        ));
    }

    fn apply_strato_wip1001_parameters_default(&mut self, config: Option<Wip1001ActivationConfig>) {
        if self.strato_wip1001_parameters.is_none()
            && let Some(config) = config
        {
            self.set_strato_wip1001_parameters(config)
                .expect("built-in WIP-1001 activation config must be internally valid");
        }
    }
}

impl EthChainSpec for WorldChainSpec {
    type Header = Header;

    fn chain(&self) -> Chain {
        self.inner.chain()
    }

    fn base_fee_params_at_timestamp(&self, timestamp: u64) -> BaseFeeParams {
        self.inner.base_fee_params_at_timestamp(timestamp)
    }

    fn blob_params_at_timestamp(&self, timestamp: u64) -> Option<BlobParams> {
        self.inner.blob_params_at_timestamp(timestamp)
    }

    fn deposit_contract(&self) -> Option<&DepositContract> {
        self.inner.deposit_contract()
    }

    fn genesis_hash(&self) -> B256 {
        self.inner.genesis_hash()
    }

    fn prune_delete_limit(&self) -> usize {
        self.inner.prune_delete_limit()
    }

    fn display_hardforks(&self) -> Box<dyn core::fmt::Display> {
        let world_forks = self.inner.hardforks.forks_iter().filter(|(fork, _)| {
            !EthereumHardfork::VARIANTS
                .iter()
                .any(|h| h.name() == (*fork).name())
        });

        Box::new(DisplayHardforks::new(world_forks))
    }

    fn genesis_header(&self) -> &Self::Header {
        self.inner.genesis_header()
    }

    fn genesis(&self) -> &Genesis {
        self.inner.genesis()
    }

    fn bootnodes(&self) -> Option<Vec<NodeRecord>> {
        self.inner.bootnodes()
    }

    fn is_optimism(&self) -> bool {
        true
    }

    fn final_paris_total_difficulty(&self) -> Option<U256> {
        self.inner.final_paris_total_difficulty()
    }

    fn next_block_base_fee(&self, parent: &Header, target_timestamp: u64) -> Option<u64> {
        if WorldChainHardforks::is_jovian_active_at_timestamp(self, parent.timestamp()) {
            compute_jovian_base_fee(self, parent, target_timestamp).ok()
        } else if WorldChainHardforks::is_holocene_active_at_timestamp(self, parent.timestamp()) {
            decode_holocene_base_fee(self, parent, target_timestamp).ok()
        } else {
            self.inner.next_block_base_fee(parent, target_timestamp)
        }
    }
}

impl Hardforks for WorldChainSpec {
    fn fork<H: Hardfork>(&self, fork: H) -> ForkCondition {
        self.inner.fork(fork)
    }

    fn forks_iter(&self) -> impl Iterator<Item = (&dyn Hardfork, ForkCondition)> {
        self.inner.forks_iter()
    }

    fn fork_id(&self, head: &Head) -> ForkId {
        self.inner.fork_id(head)
    }

    fn latest_fork_id(&self) -> ForkId {
        self.inner.latest_fork_id()
    }

    fn fork_filter(&self, head: Head) -> ForkFilter {
        self.inner.fork_filter(head)
    }
}

impl EthereumHardforks for WorldChainSpec {
    fn ethereum_fork_activation(&self, fork: EthereumHardfork) -> ForkCondition {
        self.fork(fork)
    }
}

impl WorldChainHardforks for WorldChainSpec {
    fn world_chain_fork_activation(&self, fork: WorldChainHardfork) -> ForkCondition {
        self.fork(fork)
    }
}

impl OpHardforks for WorldChainSpec {
    fn op_fork_activation(&self, fork: OpHardfork) -> ForkCondition {
        match fork {
            OpHardfork::Bedrock => self.fork(WorldChainHardfork::Bedrock),
            OpHardfork::Regolith => self.fork(WorldChainHardfork::Regolith),
            OpHardfork::Canyon => self.fork(WorldChainHardfork::Canyon),
            OpHardfork::Ecotone => self.fork(WorldChainHardfork::Ecotone),
            OpHardfork::Fjord => self.fork(WorldChainHardfork::Fjord),
            OpHardfork::Granite => self.fork(WorldChainHardfork::Granite),
            OpHardfork::Holocene => self.fork(WorldChainHardfork::Holocene),
            OpHardfork::Isthmus => self.fork(WorldChainHardfork::Isthmus),
            OpHardfork::Jovian => self.fork(WorldChainHardfork::Jovian),
            _ => self.fork(fork),
        }
    }
}

impl From<OpChainSpec> for WorldChainSpec {
    fn from(value: OpChainSpec) -> Self {
        Self::from_chain_spec(value.inner)
    }
}

impl From<ChainSpec> for WorldChainSpec {
    fn from(inner: ChainSpec) -> Self {
        Self::from_chain_spec(inner)
    }
}

impl WorldChainSpec {
    fn from_chain_spec(mut inner: ChainSpec) -> Self {
        inner.hardforks = convert_op_hardforks(&inner.hardforks);
        inner.genesis_header =
            SealedHeader::seal_slow(make_op_genesis_header(&inner.genesis, &inner.hardforks));

        let mut spec = Self {
            inner,
            strato_wip1001_parameters: None,
        };
        spec.apply_world_chain_defaults();
        spec
    }
}

impl From<Genesis> for WorldChainSpec {
    fn from(genesis: Genesis) -> Self {
        Self::from_genesis_inner(genesis)
    }
}

impl WorldChainSpec {
    fn from_genesis_inner(genesis: Genesis) -> Self {
        let genesis_info = WorldGenesisInfo::extract_from(&genesis);
        let op_genesis_info = genesis_info
            .optimism_chain_info
            .genesis_info
            .unwrap_or_default();

        let hardfork_opts = [
            (EthereumHardfork::Frontier.boxed(), Some(0)),
            (
                EthereumHardfork::Homestead.boxed(),
                genesis.config.homestead_block,
            ),
            (
                EthereumHardfork::Tangerine.boxed(),
                genesis.config.eip150_block,
            ),
            (
                EthereumHardfork::SpuriousDragon.boxed(),
                genesis.config.eip155_block,
            ),
            (
                EthereumHardfork::Byzantium.boxed(),
                genesis.config.byzantium_block,
            ),
            (
                EthereumHardfork::Constantinople.boxed(),
                genesis.config.constantinople_block,
            ),
            (
                EthereumHardfork::Petersburg.boxed(),
                genesis.config.petersburg_block,
            ),
            (
                EthereumHardfork::Istanbul.boxed(),
                genesis.config.istanbul_block,
            ),
            (
                EthereumHardfork::MuirGlacier.boxed(),
                genesis.config.muir_glacier_block,
            ),
            (
                EthereumHardfork::Berlin.boxed(),
                genesis.config.berlin_block,
            ),
            (
                EthereumHardfork::London.boxed(),
                genesis.config.london_block,
            ),
            (
                EthereumHardfork::ArrowGlacier.boxed(),
                genesis.config.arrow_glacier_block,
            ),
            (
                EthereumHardfork::GrayGlacier.boxed(),
                genesis.config.gray_glacier_block,
            ),
            (
                WorldChainHardfork::Bedrock.boxed(),
                op_genesis_info.bedrock_block,
            ),
        ];

        let mut configured_hardforks = hardfork_opts
            .into_iter()
            .filter_map(|(hardfork, opt)| opt.map(|block| (hardfork, ForkCondition::Block(block))))
            .collect::<Vec<_>>();

        configured_hardforks.push((
            EthereumHardfork::Paris.boxed(),
            ForkCondition::TTD {
                activation_block_number: 0,
                total_difficulty: U256::ZERO,
                fork_block: genesis.config.merge_netsplit_block,
            },
        ));

        let time_hardfork_opts = [
            (
                EthereumHardfork::Shanghai.boxed(),
                op_genesis_info.canyon_time,
            ),
            (
                EthereumHardfork::Cancun.boxed(),
                op_genesis_info.ecotone_time,
            ),
            (
                EthereumHardfork::Prague.boxed(),
                op_genesis_info.isthmus_time,
            ),
            (
                WorldChainHardfork::Regolith.boxed(),
                op_genesis_info.regolith_time,
            ),
            (
                WorldChainHardfork::Canyon.boxed(),
                op_genesis_info.canyon_time,
            ),
            (
                WorldChainHardfork::Ecotone.boxed(),
                op_genesis_info.ecotone_time,
            ),
            (
                WorldChainHardfork::Fjord.boxed(),
                op_genesis_info.fjord_time,
            ),
            (
                WorldChainHardfork::Granite.boxed(),
                op_genesis_info.granite_time,
            ),
            (
                WorldChainHardfork::Holocene.boxed(),
                op_genesis_info.holocene_time,
            ),
            (
                WorldChainHardfork::Isthmus.boxed(),
                op_genesis_info.isthmus_time,
            ),
            (
                WorldChainHardfork::Jovian.boxed(),
                op_genesis_info.jovian_time,
            ),
            (WorldChainHardfork::Tropo.boxed(), genesis_info.tropo_time),
            (WorldChainHardfork::Strato.boxed(), genesis_info.strato_time),
        ];

        configured_hardforks.extend(time_hardfork_opts.into_iter().filter_map(
            |(hardfork, opt)| opt.map(|time| (hardfork, ForkCondition::Timestamp(time))),
        ));

        let hardforks = order_world_hardforks(configured_hardforks);
        let genesis_header = SealedHeader::seal_slow(make_op_genesis_header(&genesis, &hardforks));

        let mut spec = Self {
            inner: ChainSpec {
                chain: genesis.config.chain_id.into(),
                genesis_header,
                genesis,
                hardforks,
                paris_block_and_final_difficulty: Some((0, U256::ZERO)),
                base_fee_params: genesis_info.base_fee_params,
                ..Default::default()
            },
            strato_wip1001_parameters: None,
        };
        spec.apply_world_chain_defaults();
        spec
    }
}

#[derive(Default, Debug)]
struct WorldGenesisInfo {
    optimism_chain_info: op_alloy_rpc_types::OpChainInfo,
    base_fee_params: BaseFeeParamsKind,
    strato_time: Option<u64>,
    tropo_time: Option<u64>,
}

impl WorldGenesisInfo {
    fn extract_from(genesis: &Genesis) -> Self {
        let mut info = Self {
            optimism_chain_info: op_alloy_rpc_types::OpChainInfo::extract_from(
                &genesis.config.extra_fields,
            )
            .unwrap_or_default(),
            tropo_time: extra_timestamp(genesis, "tropoTime"),
            strato_time: extra_timestamp(genesis, "stratoTime"),
            ..Default::default()
        };

        if let Some(optimism_base_fee_info) = &info.optimism_chain_info.base_fee_info
            && let (Some(elasticity), Some(denominator)) = (
                optimism_base_fee_info.eip1559_elasticity,
                optimism_base_fee_info.eip1559_denominator,
            )
        {
            let base_fee_params = optimism_base_fee_info
                .eip1559_denominator_canyon
                .map_or_else(
                    || BaseFeeParams::new(denominator as u128, elasticity as u128).into(),
                    |canyon_denominator| {
                        BaseFeeParamsKind::Variable(
                            vec![
                                (
                                    EthereumHardfork::London.boxed(),
                                    BaseFeeParams::new(denominator as u128, elasticity as u128),
                                ),
                                (
                                    WorldChainHardfork::Canyon.boxed(),
                                    BaseFeeParams::new(
                                        canyon_denominator as u128,
                                        elasticity as u128,
                                    ),
                                ),
                            ]
                            .into(),
                        )
                    },
                );

            info.base_fee_params = base_fee_params;
        }

        info
    }
}

fn extra_timestamp(genesis: &Genesis, key: &str) -> Option<u64> {
    match genesis.config.extra_fields.get_deserialized::<u64>(key) {
        Some(Ok(ts)) => Some(ts),
        Some(Err(err)) => {
            tracing::warn!(
                target: "world_chain::chainspec",
                %err,
                key,
                "ignoring genesis extra field: failed to deserialize as u64 timestamp"
            );
            None
        }
        None => None,
    }
}

fn convert_op_hardforks(hardforks: &ChainHardforks) -> ChainHardforks {
    ChainHardforks::new(
        hardforks
            .forks_iter()
            .map(|(fork, condition)| (convert_op_hardfork(fork), condition))
            .collect(),
    )
}

fn convert_op_hardfork(fork: &dyn Hardfork) -> Box<dyn Hardfork> {
    match fork.name() {
        "Bedrock" => WorldChainHardfork::Bedrock.boxed(),
        "Regolith" => WorldChainHardfork::Regolith.boxed(),
        "Canyon" => WorldChainHardfork::Canyon.boxed(),
        "Ecotone" => WorldChainHardfork::Ecotone.boxed(),
        "Fjord" => WorldChainHardfork::Fjord.boxed(),
        "Granite" => WorldChainHardfork::Granite.boxed(),
        "Holocene" => WorldChainHardfork::Holocene.boxed(),
        "Isthmus" => WorldChainHardfork::Isthmus.boxed(),
        "Jovian" => WorldChainHardfork::Jovian.boxed(),
        "Karst" => OpHardfork::Karst.boxed(),
        "Interop" => OpHardfork::Interop.boxed(),
        other if other.eq_ignore_ascii_case("tropo") => WorldChainHardfork::Tropo.boxed(),
        other if other.eq_ignore_ascii_case("strato") => WorldChainHardfork::Strato.boxed(),
        _ => EthereumHardfork::VARIANTS
            .iter()
            .find(|hardfork| hardfork.name() == fork.name())
            .map_or_else(
                || Box::new(UnknownHardfork(fork.name())) as Box<dyn Hardfork>,
                |hardfork| hardfork.boxed(),
            ),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UnknownHardfork(&'static str);

impl Hardfork for UnknownHardfork {
    fn name(&self) -> &'static str {
        self.0
    }
}

fn order_world_hardforks(
    mut configured: Vec<(Box<dyn Hardfork>, ForkCondition)>,
) -> ChainHardforks {
    let order = OP_MAINNET_HARDFORKS
        .forks_iter()
        .map(|(fork, _)| convert_op_hardfork(fork))
        .chain([
            WorldChainHardfork::Tropo.boxed(),
            WorldChainHardfork::Strato.boxed(),
        ]);

    let mut ordered_hardforks = Vec::with_capacity(configured.len());
    for hardfork in order {
        if let Some(pos) = configured
            .iter()
            .position(|(candidate, _)| **candidate == *hardfork)
        {
            ordered_hardforks.push(configured.remove(pos));
        }
    }

    ordered_hardforks.append(&mut configured);
    ChainHardforks::new(ordered_hardforks)
}

#[cfg(test)]
mod tests {
    use alloy_genesis::Genesis;
    use alloy_primitives::Address;
    use reth_chainspec::Hardforks;

    use super::*;
    use crate::{
        STRATO_WIP1001_PLACEHOLDER_CONFIG, WorldChainSpecBuilder,
        strato_wip1001_parameters_for_chain,
    };

    #[test]
    fn world_mainnet_defaults_to_jovian() {
        let spec = WorldChainSpec::mainnet();
        assert_eq!(
            spec.fork(WorldChainHardfork::Jovian),
            ForkCondition::Timestamp(JOVIAN_UPGRADE_TIMESTAMP_MAINNET)
        );
    }

    #[test]
    fn world_specific_hardforks_default_inactive_for_custom_genesis() {
        let spec = WorldChainSpec::from_genesis(Genesis::default());
        assert_eq!(spec.fork(WorldChainHardfork::Tropo), ForkCondition::Never);
        assert_eq!(spec.fork(WorldChainHardfork::Strato), ForkCondition::Never);
        assert_eq!(spec.strato_wip1001_parameters_at_timestamp(0), None);
    }

    #[test]
    fn strato_time_alone_does_not_activate_wip1001() {
        let mut genesis = Genesis::default();
        genesis
            .config
            .extra_fields
            .insert_value("stratoTime".to_string(), 10u64)
            .unwrap();

        let spec = WorldChainSpec::from_genesis(genesis);
        assert_eq!(
            spec.fork(WorldChainHardfork::Strato),
            ForkCondition::Timestamp(10)
        );
        assert!(spec.is_strato_active_at_timestamp(10));
        assert_eq!(spec.strato_wip1001_parameters_at_timestamp(10), None);
    }

    #[test]
    fn strato_readiness_rejects_scheduled_fork_without_wip1001_config() {
        let spec = WorldChainSpecBuilder::default()
            .chain(Chain::from_id(1))
            .genesis(Genesis::default())
            .strato_activated()
            .build();

        assert_eq!(
            spec.validate_wip1001_activation_readiness(),
            Err(Wip1001ActivationReadinessError::StratoScheduledWithoutConfig)
        );
    }

    #[test]
    fn wip1001_builder_try_build_applies_strato_readiness_gate() {
        let err = WorldChainSpecBuilder::default()
            .chain(Chain::from_id(1))
            .genesis(Genesis::default())
            .strato_activated()
            .try_build()
            .expect_err("scheduled Strato needs WIP-1001 parameters");

        assert_eq!(
            err,
            Wip1001ActivationReadinessError::StratoScheduledWithoutConfig
        );

        WorldChainSpecBuilder::default()
            .chain(Chain::from_id(1))
            .genesis(Genesis::default())
            .strato_activated()
            .with_strato_wip1001_parameters(STRATO_WIP1001_PLACEHOLDER_CONFIG)
            .expect("placeholder config is valid")
            .try_build()
            .expect("complete config is ready");
    }

    #[test]
    fn strato_readiness_accepts_unscheduled_or_fully_configured_wip1001() {
        let unscheduled = WorldChainSpecBuilder::default()
            .chain(Chain::from_id(1))
            .genesis(Genesis::default())
            .build();
        assert_eq!(unscheduled.validate_wip1001_activation_readiness(), Ok(()));

        let configured = WorldChainSpecBuilder::default()
            .chain(Chain::from_id(1))
            .genesis(Genesis::default())
            .strato_activated()
            .with_strato_wip1001_parameters(STRATO_WIP1001_PLACEHOLDER_CONFIG)
            .expect("placeholder config is valid")
            .build();
        assert_eq!(configured.validate_wip1001_activation_readiness(), Ok(()));
    }

    #[test]
    fn strato_readiness_rejects_custom_wip1001_config_on_world_chains() {
        for chain in [Chain::from_id(480), Chain::from_id(4801)] {
            let mut config = strato_wip1001_parameters_for_chain(chain)
                .unwrap_or(STRATO_WIP1001_PLACEHOLDER_CONFIG);
            config.block_validation_gas_budget += 1;

            let spec = WorldChainSpecBuilder::default()
                .chain(chain)
                .genesis(Genesis::default())
                .with_strato_wip1001_parameters(config)
                .expect("custom config is valid")
                .build();

            assert_eq!(
                spec.validate_wip1001_activation_readiness(),
                Err(Wip1001ActivationReadinessError::ProductionConfigMismatch)
            );
        }
    }

    #[test]
    fn strato_readiness_allows_placeholder_config_on_custom_chains() {
        let spec = WorldChainSpecBuilder::default()
            .chain(Chain::from_id(1))
            .genesis(Genesis::default())
            .with_strato_wip1001_parameters(STRATO_WIP1001_PLACEHOLDER_CONFIG)
            .expect("placeholder config is valid")
            .build();

        assert_eq!(spec.validate_wip1001_activation_readiness(), Ok(()));
    }

    #[test]
    fn strato_wip1001_parameters_is_available_only_after_strato() {
        let mut genesis = Genesis::default();
        genesis
            .config
            .extra_fields
            .insert_value("stratoTime".to_string(), 10u64)
            .unwrap();

        let mut spec = WorldChainSpec::from_genesis(genesis);
        spec.set_strato_wip1001_parameters(STRATO_WIP1001_PLACEHOLDER_CONFIG)
            .expect("placeholder config is valid");

        assert_eq!(spec.strato_wip1001_parameters_at_timestamp(9), None);
        assert_eq!(
            spec.strato_wip1001_parameters_at_timestamp(10),
            Some(&STRATO_WIP1001_PLACEHOLDER_CONFIG)
        );
    }

    #[test]
    fn from_chain_spec_does_not_parse_wip1001_config_from_inner_genesis() {
        let spec = WorldChainSpec::from(ChainSpec {
            chain: Chain::from_id(1),
            genesis: Genesis::default(),
            ..Default::default()
        });

        assert_eq!(spec.strato_wip1001_parameters_at_timestamp(0), None);
    }

    #[test]
    fn spec_setter_replaces_wip1001_config() {
        let mut spec = WorldChainSpec::from_genesis(Genesis::default());
        spec.set_fork(WorldChainHardfork::Strato, ForkCondition::Timestamp(0));
        spec.set_strato_wip1001_parameters(STRATO_WIP1001_PLACEHOLDER_CONFIG)
            .expect("placeholder config is valid");

        assert_eq!(
            spec.strato_wip1001_parameters_at_timestamp(0),
            Some(&STRATO_WIP1001_PLACEHOLDER_CONFIG)
        );
        assert_eq!(spec.validate_wip1001_activation_readiness(), Ok(()));
    }

    #[test]
    fn builder_can_enable_placeholder_wip1001_config() {
        let spec = WorldChainSpecBuilder::default()
            .chain(Chain::from_id(1))
            .genesis(Genesis::default())
            .strato_activated()
            .with_strato_wip1001_parameters(STRATO_WIP1001_PLACEHOLDER_CONFIG)
            .expect("placeholder config is valid")
            .build();

        assert!(spec.is_strato_active_at_timestamp(0));
        assert_eq!(
            spec.strato_wip1001_parameters_at_timestamp(0),
            Some(&STRATO_WIP1001_PLACEHOLDER_CONFIG)
        );
    }

    #[test]
    fn builder_rejects_invalid_wip1001_config() {
        let mut invalid = STRATO_WIP1001_PLACEHOLDER_CONFIG;
        invalid.block_validation_gas_budget = 1;

        let err = WorldChainSpecBuilder::default()
            .with_strato_wip1001_parameters(invalid)
            .expect_err("invalid config must be rejected");

        assert_eq!(
            err,
            Wip1001ActivationConfigError::ValidationBudgetTooLow {
                minimum: invalid.eip1271_validation_gas_limit
                    + invalid.execution_trace_validation_gas_limit,
                actual: invalid.block_validation_gas_budget,
            }
        );
    }

    #[test]
    fn spec_setter_rejects_invalid_wip1001_config_without_mutation() {
        let mut spec = WorldChainSpecBuilder::default()
            .chain(Chain::from_id(1))
            .genesis(Genesis::default())
            .strato_activated()
            .with_strato_wip1001_parameters(STRATO_WIP1001_PLACEHOLDER_CONFIG)
            .expect("placeholder config is valid")
            .build();

        let mut invalid = STRATO_WIP1001_PLACEHOLDER_CONFIG;
        invalid.world_chain_account_manager = Address::ZERO;

        let err = spec
            .set_strato_wip1001_parameters(invalid)
            .expect_err("invalid config must be rejected");

        assert_eq!(
            err,
            Wip1001ActivationConfigError::ZeroAddress("world_chain_account_manager")
        );
        assert_eq!(
            spec.strato_wip1001_parameters_at_timestamp(0),
            Some(&STRATO_WIP1001_PLACEHOLDER_CONFIG)
        );
    }
}
