use std::sync::Arc;

use alloy_consensus::{BlockHeader, Header};
use alloy_primitives::{Address, B64};
use reth_chainspec::{BaseFeeParams, EthereumHardforks};
use reth_node_api::PayloadAttributesBuilder;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::OpPayloadAttributes;
use reth_optimism_payload_builder::OpPayloadAttrs;
use reth_primitives_traits::SealedHeader;

/// Builds [`OpPayloadAttrs`] for local/dev-mode payload generation.
///
/// Mirrors `reth_optimism_node::node::OpLocalPayloadAttributesBuilder`, which is
/// not re-exported by upstream. Used by [`DebugNode`] when running in dev mode.
pub(crate) struct OpLocalPayloadAttributesBuilder<ChainSpec> {
    pub(crate) chain_spec: Arc<ChainSpec>,
}

impl<ChainSpec> PayloadAttributesBuilder<OpPayloadAttrs>
    for OpLocalPayloadAttributesBuilder<ChainSpec>
where
    ChainSpec: EthereumHardforks + OpHardforks + Send + Sync + 'static,
{
    fn build(&self, parent: &SealedHeader<Header>) -> OpPayloadAttrs {
        let timestamp = std::cmp::max(
            parent.timestamp().saturating_add(1),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );

        let eth_attrs = alloy_rpc_types_engine::PayloadAttributes {
            timestamp,
            prev_randao: alloy_primitives::B256::random(),
            suggested_fee_recipient: Address::random(),
            withdrawals: self
                .chain_spec
                .is_shanghai_active_at_timestamp(timestamp)
                .then(Default::default),
            parent_beacon_block_root: self
                .chain_spec
                .is_cancun_active_at_timestamp(timestamp)
                .then(alloy_primitives::B256::random),
            slot_number: self
                .chain_spec
                .is_amsterdam_active_at_timestamp(timestamp)
                .then_some(0),
        };

        // Dummy system transaction for dev mode.
        // OP Mainnet transaction at index 0 in block 124665056.
        const TX_SET_L1_BLOCK: [u8; 251] = alloy_primitives::hex!(
            "7ef8f8a0683079df94aa5b9cf86687d739a60a9b4f0835e520ec4d664e2e415dca17a6df94deaddeaddeaddeaddeaddeaddeaddeaddead00019442000000000000000000000000000000000000158080830f424080b8a4440a5e200000146b000f79c500000000000000040000000066d052e700000000013ad8a3000000000000000000000000000000000000000000000000000000003ef1278700000000000000000000000000000000000000000000000000000000000000012fdf87b89884a61e74b322bbcf60386f543bfae7827725efaaf0ab1de2294a590000000000000000000000006887246668a3b87f54deb3b94ba47a6f63f32985"
        );

        let default_params = BaseFeeParams::optimism();
        let denominator = std::env::var("OP_DEV_EIP1559_DENOMINATOR")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(default_params.max_change_denominator as u32);
        let elasticity = std::env::var("OP_DEV_EIP1559_ELASTICITY")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(default_params.elasticity_multiplier as u32);
        let gas_limit = std::env::var("OP_DEV_GAS_LIMIT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok());

        let mut eip1559_bytes = [0u8; 8];
        eip1559_bytes[0..4].copy_from_slice(&denominator.to_be_bytes());
        eip1559_bytes[4..8].copy_from_slice(&elasticity.to_be_bytes());

        OpPayloadAttrs(OpPayloadAttributes {
            payload_attributes: eth_attrs,
            transactions: Some(vec![TX_SET_L1_BLOCK.into()]),
            no_tx_pool: None,
            gas_limit,
            eip_1559_params: Some(B64::from(eip1559_bytes)),
            min_base_fee: self
                .chain_spec
                .is_jovian_active_at_timestamp(timestamp)
                .then_some(0),
        })
    }
}
