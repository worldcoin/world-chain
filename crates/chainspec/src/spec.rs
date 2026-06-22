use std::{boxed::Box, sync::Arc, vec, vec::Vec};

use alloy_chains::{Chain, NamedChain};
use alloy_consensus::{BlockHeader, Header};
use alloy_eips::eip7840::BlobParams;
use alloy_genesis::Genesis;
use alloy_hardforks::Hardfork;
use alloy_primitives::{B256, U256};
use derive_more::{Constructor, Deref, Into};
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
use reth_optimism_forks::{OpHardfork, OpHardforks};
use reth_primitives_traits::SealedHeader;

use crate::{WorldChainHardfork, WorldChainHardforks};

/// World Chain Jovian activation timestamp on Sepolia.
pub const JOVIAN_UPGRADE_TIMESTAMP_SEPOLIA: u64 = 1_777_161_600;

/// World Chain Jovian activation timestamp on mainnet.
pub const JOVIAN_UPGRADE_TIMESTAMP_MAINNET: u64 = 1_777_593_600;

/// World Chain spec type.
///
/// This wraps reth's generic [`ChainSpec`] the same way the OP stack spec does, while using World
/// Chain hardfork names as the canonical post-Karst schedule.
#[derive(Debug, Clone, Deref, Into, Constructor, PartialEq, Eq)]
pub struct WorldChainSpec {
    /// Inner reth chain spec.
    pub inner: ChainSpec,
}

impl WorldChainSpec {
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
        let Some(fork) = convert_op_hardfork(&fork) else {
            return;
        };
        self.inner.hardforks.insert(fork, condition);
        self.inner.genesis_header = SealedHeader::seal_slow(make_op_genesis_header(
            &self.inner.genesis,
            &self.inner.hardforks,
        ));
    }

    /// Applies World Chain defaults that are not yet represented in the upstream OP stack chain
    /// specs. Only fills in forks that have no explicit activation; an operator-supplied
    /// timestamp (e.g. `jovianTime` in genesis) is preserved.
    pub fn apply_world_chain_defaults(&mut self) {
        if !matches!(
            self.inner.fork(WorldChainHardfork::Jovian),
            ForkCondition::Never
        ) {
            return;
        }
        match self.chain().named() {
            Some(NamedChain::World) => self.set_fork(
                WorldChainHardfork::Jovian,
                ForkCondition::Timestamp(JOVIAN_UPGRADE_TIMESTAMP_MAINNET),
            ),
            Some(NamedChain::WorldSepolia) => self.set_fork(
                WorldChainHardfork::Jovian,
                ForkCondition::Timestamp(JOVIAN_UPGRADE_TIMESTAMP_SEPOLIA),
            ),
            _ => {}
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
            compute_jovian_base_fee(parent).ok()
        } else if WorldChainHardforks::is_holocene_active_at_timestamp(self, parent.timestamp()) {
            decode_holocene_base_fee(parent).ok()
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
            OpHardfork::Karst => self.fork(WorldChainHardfork::Karst),
            _ => ForkCondition::Never,
        }
    }
}

impl From<OpChainSpec> for WorldChainSpec {
    fn from(value: OpChainSpec) -> Self {
        let mut inner = value.inner;
        inner.hardforks = convert_op_hardforks(&inner.hardforks);
        inner.genesis_header =
            SealedHeader::seal_slow(make_op_genesis_header(&inner.genesis, &inner.hardforks));

        let mut spec = Self { inner };
        spec.apply_world_chain_defaults();
        spec
    }
}

impl From<ChainSpec> for WorldChainSpec {
    fn from(mut inner: ChainSpec) -> Self {
        inner.hardforks = convert_op_hardforks(&inner.hardforks);
        inner.genesis_header =
            SealedHeader::seal_slow(make_op_genesis_header(&inner.genesis, &inner.hardforks));

        let mut spec = Self { inner };
        spec.apply_world_chain_defaults();
        spec
    }
}

impl From<Genesis> for WorldChainSpec {
    fn from(genesis: Genesis) -> Self {
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
            (EthereumHardfork::Osaka.boxed(), op_genesis_info.karst_time),
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
            (
                WorldChainHardfork::Karst.boxed(),
                op_genesis_info.karst_time,
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
        };
        spec.apply_world_chain_defaults();
        spec
    }
}

#[derive(Default, Debug)]
struct WorldGenesisInfo {
    optimism_chain_info: op_alloy_rpc_types::OpChainInfo,
    base_fee_params: BaseFeeParamsKind,
    tropo_time: Option<u64>,
    strato_time: Option<u64>,
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

pub(crate) fn convert_op_hardforks(hardforks: &ChainHardforks) -> ChainHardforks {
    let mut converted = ChainHardforks::default();
    for (fork, condition) in hardforks.forks_iter() {
        let Some(fork) = convert_op_hardfork(fork) else {
            continue;
        };
        converted.insert(fork, condition);
    }
    converted
}

pub(crate) fn convert_op_hardfork(fork: &dyn Hardfork) -> Option<Box<dyn Hardfork>> {
    match fork.name() {
        "Bedrock" => Some(WorldChainHardfork::Bedrock.boxed()),
        "Regolith" => Some(WorldChainHardfork::Regolith.boxed()),
        "Canyon" => Some(WorldChainHardfork::Canyon.boxed()),
        "Ecotone" => Some(WorldChainHardfork::Ecotone.boxed()),
        "Fjord" => Some(WorldChainHardfork::Fjord.boxed()),
        "Granite" => Some(WorldChainHardfork::Granite.boxed()),
        "Holocene" => Some(WorldChainHardfork::Holocene.boxed()),
        "Isthmus" => Some(WorldChainHardfork::Isthmus.boxed()),
        "Jovian" => Some(WorldChainHardfork::Jovian.boxed()),
        "Karst" => Some(WorldChainHardfork::Karst.boxed()),
        "Interop" => None,
        other if other.eq_ignore_ascii_case("tropo") => Some(WorldChainHardfork::Tropo.boxed()),
        other if other.eq_ignore_ascii_case("strato") => Some(WorldChainHardfork::Strato.boxed()),
        _ => EthereumHardfork::VARIANTS
            .iter()
            .find(|hardfork| hardfork.name() == fork.name())
            .map_or_else(
                || Some(Box::new(UnknownHardfork(fork.name())) as Box<dyn Hardfork>),
                |hardfork| Some(hardfork.boxed()),
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
    let order = [
        WorldChainHardfork::Bedrock.boxed(),
        WorldChainHardfork::Regolith.boxed(),
        WorldChainHardfork::Canyon.boxed(),
        WorldChainHardfork::Ecotone.boxed(),
        WorldChainHardfork::Fjord.boxed(),
        WorldChainHardfork::Granite.boxed(),
        WorldChainHardfork::Holocene.boxed(),
        WorldChainHardfork::Isthmus.boxed(),
        WorldChainHardfork::Jovian.boxed(),
        WorldChainHardfork::Karst.boxed(),
        WorldChainHardfork::Tropo.boxed(),
        WorldChainHardfork::Strato.boxed(),
    ];

    let mut ordered_hardforks = Vec::with_capacity(configured.len());
    for hardfork in order.into_iter() {
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
    use reth_chainspec::Hardforks;
    use reth_optimism_forks::OpHardforks;

    use crate::WorldChainSpecBuilder;

    use super::*;

    #[test]
    fn world_mainnet_defaults_to_jovian() {
        let spec = WorldChainSpec::mainnet();
        assert_eq!(
            spec.fork(WorldChainHardfork::Jovian),
            ForkCondition::Timestamp(JOVIAN_UPGRADE_TIMESTAMP_MAINNET)
        );
        assert_eq!(spec.fork(WorldChainHardfork::Karst), ForkCondition::Never);
    }

    #[test]
    fn post_jovian_hardforks_default_inactive_for_custom_genesis() {
        let spec = WorldChainSpec::from_genesis(Genesis::default());
        assert_eq!(spec.fork(WorldChainHardfork::Karst), ForkCondition::Never);
        assert_eq!(spec.fork(WorldChainHardfork::Tropo), ForkCondition::Never);
        assert_eq!(spec.fork(WorldChainHardfork::Strato), ForkCondition::Never);
    }

    #[test]
    fn world_specific_hardforks_keep_karst_activation() {
        let spec = WorldChainSpecBuilder::mainnet()
            .jovian_activated()
            .karst_activated()
            .tropo_activated()
            .strato_activated()
            .build();

        assert_eq!(
            spec.op_fork_activation(OpHardfork::Karst),
            ForkCondition::Timestamp(0)
        );
        assert_eq!(
            spec.fork(EthereumHardfork::Osaka),
            ForkCondition::Timestamp(0)
        );
    }

    #[test]
    fn converting_op_specs_preserves_karst() {
        let mut spec = WorldChainSpec::from_genesis(Genesis::default());
        spec.set_fork(OpHardfork::Karst, ForkCondition::Timestamp(10));

        let converted = WorldChainSpec::from(spec.inner);

        assert_eq!(
            converted.fork(WorldChainHardfork::Karst),
            ForkCondition::Timestamp(10)
        );
        assert_eq!(
            converted.op_fork_activation(OpHardfork::Karst),
            ForkCondition::Timestamp(10)
        );
    }

    #[test]
    fn world_hardfork_order_places_karst_before_world_specific_forks() {
        let hardforks = order_world_hardforks(vec![
            (
                WorldChainHardfork::Strato.boxed(),
                ForkCondition::Timestamp(30),
            ),
            (
                WorldChainHardfork::Tropo.boxed(),
                ForkCondition::Timestamp(20),
            ),
            (
                WorldChainHardfork::Jovian.boxed(),
                ForkCondition::Timestamp(10),
            ),
            (
                WorldChainHardfork::Karst.boxed(),
                ForkCondition::Timestamp(15),
            ),
        ]);
        let names = hardforks
            .forks_iter()
            .map(|(fork, _)| fork.name())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["Jovian", "Karst", "Tropo", "Strato"]);
    }

    #[test]
    fn builder_preserves_karst_from_generic_inputs() {
        let spec = WorldChainSpecBuilder::mainnet()
            .with_fork(OpHardfork::Karst, ForkCondition::Timestamp(10))
            .with_fork(OpHardfork::Jovian, ForkCondition::Timestamp(5))
            .build();

        assert_eq!(
            spec.fork(WorldChainHardfork::Karst),
            ForkCondition::Timestamp(10)
        );
        assert_eq!(
            spec.fork(WorldChainHardfork::Jovian),
            ForkCondition::Timestamp(5)
        );
    }
}
