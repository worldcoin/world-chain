use alloy_chains::Chain;
use alloy_op_hardforks::OpHardfork;
use alloy_primitives::{b256, U256};
use reth::{
    chainspec::{
        BaseFeeParams, BaseFeeParamsKind, ChainHardforks, ChainSpec, EthereumHardfork,
        ForkCondition, Hardfork,
    },
    primitives::SealedHeader,
};
use reth_optimism_chainspec::{make_op_genesis_header, OpChainSpec};
use std::sync::{Arc, LazyLock};

pub const CHAIN_ID: u64 = 69420;

/// The Alchemy Sepolia spec
pub static ALCHEMY_SEPOLIA: LazyLock<Arc<OpChainSpec>> = LazyLock::new(|| {
    let genesis = serde_json::from_str(include_str!("../res/alchemy_sepolia.json"))
        .expect("Can't deserialize Alchemy Sepolia genesis json");
    let hardforks = ALCHEMY_SEPOLIA_HARDFORKS.clone();
    OpChainSpec {
        inner: ChainSpec {
            chain: Chain::from(CHAIN_ID),
            genesis_header: SealedHeader::new(
                make_op_genesis_header(&genesis, &hardforks),
                b256!("257d26bac7028be25e0ed40c496325ab505818709f0b8d58b12205aa4ca972f4"),
            ),
            genesis,
            paris_block_and_final_difficulty: Some((0, U256::from(0))),
            hardforks,
            base_fee_params: BaseFeeParamsKind::Variable(
                vec![
                    (EthereumHardfork::London.boxed(), BaseFeeParams::new(50, 10)),
                    (OpHardfork::Canyon.boxed(), BaseFeeParams::new(250, 10)),
                ]
                .into(),
            ),
            ..Default::default()
        },
    }
    .into()
});

/// Alchemy Sepolia list of hardforks.
pub static ALCHEMY_SEPOLIA_HARDFORKS: LazyLock<ChainHardforks> = LazyLock::new(|| {
    ChainHardforks::new(vec![
        (EthereumHardfork::Frontier.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::Homestead.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::Tangerine.boxed(), ForkCondition::Block(0)),
        (
            EthereumHardfork::SpuriousDragon.boxed(),
            ForkCondition::Block(0),
        ),
        (EthereumHardfork::Byzantium.boxed(), ForkCondition::Block(0)),
        (
            EthereumHardfork::Constantinople.boxed(),
            ForkCondition::Block(0),
        ),
        (
            EthereumHardfork::Petersburg.boxed(),
            ForkCondition::Block(0),
        ),
        (EthereumHardfork::Istanbul.boxed(), ForkCondition::Block(0)),
        (
            EthereumHardfork::MuirGlacier.boxed(),
            ForkCondition::Block(0),
        ),
        (EthereumHardfork::Berlin.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::London.boxed(), ForkCondition::Block(0)),
        (
            EthereumHardfork::ArrowGlacier.boxed(),
            ForkCondition::Block(0),
        ),
        (
            EthereumHardfork::GrayGlacier.boxed(),
            ForkCondition::Block(0),
        ),
        (
            EthereumHardfork::Paris.boxed(),
            ForkCondition::TTD {
                activation_block_number: 0,
                fork_block: Some(0),
                total_difficulty: U256::ZERO,
            },
        ),
        (OpHardfork::Bedrock.boxed(), ForkCondition::Block(0)),
        (OpHardfork::Regolith.boxed(), ForkCondition::Timestamp(0)),
        (
            EthereumHardfork::Shanghai.boxed(),
            ForkCondition::Timestamp(0),
        ),
        (
            OpHardfork::Canyon.boxed(),
            ForkCondition::Timestamp(1717675200),
        ),
        (
            EthereumHardfork::Cancun.boxed(),
            ForkCondition::Timestamp(0),
        ),
        (
            OpHardfork::Ecotone.boxed(),
            ForkCondition::Timestamp(1717675200),
        ),
        (
            OpHardfork::Fjord.boxed(),
            ForkCondition::Timestamp(1721309400),
        ),
        (
            OpHardfork::Granite.boxed(),
            ForkCondition::Timestamp(1727110800),
        ),
        (
            OpHardfork::Holocene.boxed(),
            ForkCondition::Timestamp(1737540000),
        ),
    ])
});
