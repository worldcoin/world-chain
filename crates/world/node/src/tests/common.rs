use crate::{flashblocks::WorldChainFlashblocksNode, tests::WorldChainNode};
use alloy_eips::Encodable2718;
use alloy_rpc_types_engine::{ForkchoiceState, ForkchoiceUpdated, PayloadAttributes};
use op_alloy_consensus::OpTxEnvelope;
use reth::chainspec::EthChainSpec;
use reth_e2e_test_utils::transaction::TransactionTestContext;
use reth_node_api::{PayloadBuilderAttributes, PayloadKind};
use reth_optimism_node::{
    OpBuiltPayload, OpEngineTypes, OpPayloadAttributes, OpPayloadBuilderAttributes,
};
use reth_optimism_primitives::OpPrimitives;

use revm_primitives::{Address, B256};
use rollup_boost::Authorization;
use tracing::info;
use world_chain_builder_flashblocks::rpc::engine::FlashblocksEngineApiExtClient;
use world_chain_builder_test_utils::utils::signer;

pub fn attributes(timestamp: u64) -> Option<OpPayloadAttributes> {
    Some(OpPayloadAttributes {
        payload_attributes: PayloadAttributes {
            timestamp,
            prev_randao: B256::ZERO,
            suggested_fee_recipient: Address::ZERO,
            withdrawals: Some(vec![]),
            parent_beacon_block_root: Some(B256::ZERO),
        },
        transactions: vec![].into(),
        no_tx_pool: Some(false),
        gas_limit: Some(30_000_000),
        eip_1559_params: None,
    })
}

pub struct EngineDriver {
    pub timestamp: u64,
    pub gen: fn(u64) -> Option<OpPayloadAttributes>,
}

impl Default for EngineDriver {
    fn default() -> Self {
        Self {
            timestamp: 1710338135,
            gen: attributes,
        }
    }
}

impl EngineDriver {
    pub async fn drive(
        &mut self,
        transaction_count: usize,
        authorization: impl Fn(OpPayloadBuilderAttributes<OpTxEnvelope>) -> Authorization + Copy,
        builder: &mut WorldChainNode<WorldChainFlashblocksNode>,
        current_head: B256,
    ) -> eyre::Result<Option<OpBuiltPayload<OpPrimitives>>> {
        self.timestamp += 1;

        let attrs = (self.gen)(self.timestamp);

        let builder_attrs =
            OpPayloadBuilderAttributes::try_new(current_head, attrs.clone().unwrap_or_default(), 3)
                .unwrap();

        let authorization = authorization(builder_attrs.clone());

        let client = builder.inner.auth_server_handle().http_client();

        info!(target: "payload_builder", id=%builder_attrs.payload_id(), "submitting forkchoice update with new payload attributes");

        let result: ForkchoiceUpdated =
            FlashblocksEngineApiExtClient::<OpEngineTypes>::flashblocks_fork_choice_updated_v3(
                &client,
                ForkchoiceState {
                    head_block_hash: current_head, // We don't change the head here
                    safe_block_hash: current_head,
                    finalized_block_hash: B256::ZERO,
                },
                attrs.clone(),
                if attrs.as_ref().is_some() {
                    Some(authorization)
                } else {
                    None
                },
            )
            .await
            .map_err(|e| eyre::eyre::eyre!(e))
            .expect("Forkchoice update failed");

        let mut transactions = vec![];
        for i in 0..transaction_count {
            let tx = TransactionTestContext::transfer_tx(
                builder.inner.chain_spec().chain_id(),
                signer(i as u32),
            )
            .await;
            let envelope = TransactionTestContext::sign_tx(signer(i as u32), tx.into()).await;
            let tx_hash = builder
                .rpc
                .inject_tx(envelope.encoded_2718().into())
                .await?;
            transactions.push(tx_hash);
        }

        tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
        if let Some(_) = attrs {
            let payload = builder
                .inner
                .payload_builder_handle
                .resolve_kind(
                    result.payload_id.expect("payload_id missing"),
                    PayloadKind::Earliest,
                )
                .await
                .unwrap()
                .expect("Failed to resolve payload");

            Ok(Some(payload))
        } else {
            Ok(None)
        }
    }
}
