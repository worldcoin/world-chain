use crate::payload::{
    ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1, FlashblocksPayloadV1,
};
use crate::primitives::reth::ExecutionInfo;
use alloy_consensus::{Eip658Value, Header, Transaction, Typed2718, EMPTY_OMMER_ROOT_HASH};
use alloy_eips::{merge::BEACON_NONCE, Encodable2718};
use alloy_primitives::{map::HashMap, Address, Bytes, B256, U256};
use alloy_rpc_types_eth::Withdrawals;
use futures_util::{FutureExt, SinkExt};
use reth::{
    builder::{
        components::{PayloadBuilderBuilder, PayloadServiceBuilder},
        node::FullNodeTypes,
        BuilderContext,
    },
    payload::PayloadBuilderHandle,
};
use reth_basic_payload_builder::{BasicPayloadJobGeneratorConfig, BuildOutcome, PayloadConfig};
use reth_chainspec::EthChainSpec;
use reth_evm::{
    env::EvmEnv, eth::receipt_builder::ReceiptBuilderCtx, execute::BlockBuilder, ConfigureEvm,
    Database, Evm, EvmError, InvalidTxError,
};
use reth_execution_types::ExecutionOutcome;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_consensus::calculate_receipt_root_no_memo_optimism;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::OpEngineTypes;
use reth_optimism_payload_builder::{
    error::OpPayloadBuilderError,
    payload::{OpBuiltPayload, OpPayloadBuilderAttributes},
};
use reth_optimism_primitives::{OpPrimitives, OpReceipt, OpTransactionSigned};
use reth_payload_builder_primitives::PayloadBuilderError;
use reth_payload_util::{BestPayloadTransactions, PayloadTransactions};
use reth_primitives::{BlockBody, SealedHeader};
use reth_primitives_traits::{proofs, Block as _, SignedTransaction};
use reth_provider::{
    CanonStateSubscriptions, HashedPostStateProvider, ProviderError, StateProviderFactory,
    StateRootProvider, StorageRootProvider,
};
use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction, TransactionPool};
use revm::{
    context::{result::ResultAndState, Block as _},
    database::{states::bundle_state::BundleRetention, BundleState, State},
    DatabaseCommit,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tokio_tungstenite::{accept_async, WebSocketStream};
use tracing::{debug, trace, warn};

pub fn build_block<ChainSpec, DB, P>(
    mut state: State<DB>,
    ctx: &OpPayloadBuilderCtx<ChainSpec>,
    info: &mut ExecutionInfo<OpPrimitives>,
) -> Result<(OpBuiltPayload, FlashblocksPayloadV1, BundleState), PayloadBuilderError>
where
    ChainSpec: EthChainSpec + OpHardforks,
    DB: Database<Error = ProviderError> + AsRef<P>,
    P: StateRootProvider + HashedPostStateProvider + StorageRootProvider,
{
    // TODO: We must run this only once per block, but we are running it on every flashblock
    // merge all transitions into bundle state, this would apply the withdrawal balance changes
    // and 4788 contract call
    state.merge_transitions(BundleRetention::Reverts);

    let new_bundle = state.take_bundle();

    let block_number = ctx.block_number();
    assert_eq!(block_number, ctx.parent().number + 1);

    let execution_outcome = ExecutionOutcome::new(
        new_bundle.clone(),
        vec![info.receipts.clone()],
        block_number,
        vec![],
    );
    let receipts_root = execution_outcome
        .generic_receipts_root_slow(block_number, |receipts| {
            calculate_receipt_root_no_memo_optimism(
                receipts,
                &ctx.chain_spec,
                ctx.attributes().timestamp(),
            )
        })
        .expect("Number is in range");
    let logs_bloom = execution_outcome
        .block_logs_bloom(block_number)
        .expect("Number is in range");

    // // calculate the state root
    let state_provider = state.database.as_ref();
    let hashed_state = state_provider.hashed_post_state(execution_outcome.state());
    let (state_root, _trie_output) = {
        state
            .database
            .as_ref()
            .state_root_with_updates(hashed_state.clone())
            .inspect_err(|err| {
                warn!(target: "payload_builder",
                parent_header=%ctx.parent().hash(),
                    %err,
                    "failed to calculate state root for payload"
                );
            })?
    };

    // create the block header
    let transactions_root = proofs::calculate_transaction_root(&info.executed_transactions);

    // OP doesn't support blobs/EIP-4844.
    // https://specs.optimism.io/protocol/exec-engine.html#ecotone-disable-blob-transactions
    // Need [Some] or [None] based on hardfork to match block hash.
    let (excess_blob_gas, blob_gas_used) = ctx.blob_fields();
    let extra_data = ctx.extra_data()?;

    let header = Header {
        parent_hash: ctx.parent().hash(),
        ommers_hash: EMPTY_OMMER_ROOT_HASH,
        beneficiary: ctx.evm_env.block_env.beneficiary,
        state_root,
        transactions_root,
        receipts_root,
        withdrawals_root: None,
        logs_bloom,
        timestamp: ctx.attributes().payload_attributes.timestamp,
        mix_hash: ctx.attributes().payload_attributes.prev_randao,
        nonce: BEACON_NONCE.into(),
        base_fee_per_gas: Some(ctx.base_fee()),
        number: ctx.parent().number + 1,
        gas_limit: ctx.block_gas_limit(),
        difficulty: U256::ZERO,
        gas_used: info.cumulative_gas_used,
        extra_data,
        parent_beacon_block_root: ctx.attributes().payload_attributes.parent_beacon_block_root,
        blob_gas_used,
        excess_blob_gas,
        requests_hash: None,
    };

    // seal the block
    let block = alloy_consensus::Block::<OpTransactionSigned>::new(
        header,
        BlockBody {
            transactions: info.executed_transactions.clone(),
            ommers: vec![],
            withdrawals: ctx.withdrawals().cloned(),
        },
    );

    let sealed_block = Arc::new(block.seal_slow());
    debug!(target: "payload_builder", ?sealed_block, "sealed built block");

    let block_hash = sealed_block.hash();

    // pick the new transactions from the info field and update the last flashblock index
    let new_transactions = info.executed_transactions[info.last_flashblock_index..].to_vec();

    let new_transactions_encoded = new_transactions
        .clone()
        .into_iter()
        .map(|tx| tx.encoded_2718().into())
        .collect::<Vec<_>>();

    let new_receipts = info.receipts[info.last_flashblock_index..].to_vec();
    info.last_flashblock_index = info.executed_transactions.len();
    let receipts_with_hash = new_transactions
        .iter()
        .zip(new_receipts.iter())
        .map(|(tx, receipt)| (*tx.tx_hash(), receipt.clone()))
        .collect::<HashMap<B256, OpReceipt>>();
    let new_account_balances = new_bundle
        .state
        .iter()
        .filter_map(|(address, account)| account.info.as_ref().map(|info| (*address, info.balance)))
        .collect::<HashMap<Address, U256>>();

    let metadata: FlashblocksMetadata<OpPrimitives> = FlashblocksMetadata {
        receipts: receipts_with_hash,
        new_account_balances,
        block_number: ctx.parent().number + 1,
    };

    // Prepare the flashblocks message
    let fb_payload = FlashblocksPayloadV1 {
        payload_id: ctx.payload_id(),
        index: 0,
        base: Some(ExecutionPayloadBaseV1 {
            parent_beacon_block_root: ctx
                .attributes()
                .payload_attributes
                .parent_beacon_block_root
                .unwrap(),
            parent_hash: ctx.parent().hash(),
            fee_recipient: ctx.attributes().suggested_fee_recipient(),
            prev_randao: ctx.attributes().payload_attributes.prev_randao,
            block_number: ctx.parent().number + 1,
            gas_limit: ctx.block_gas_limit(),
            timestamp: ctx.attributes().payload_attributes.timestamp,
            extra_data: ctx.extra_data()?,
            base_fee_per_gas: ctx.base_fee().try_into().unwrap(),
        }),
        diff: ExecutionPayloadFlashblockDeltaV1 {
            state_root,
            receipts_root,
            logs_bloom,
            gas_used: info.cumulative_gas_used,
            block_hash,
            transactions: new_transactions_encoded,
            withdrawals: ctx.withdrawals().cloned().unwrap_or_default().to_vec(),
        },
        metadata: serde_json::to_value(&metadata).unwrap_or_default(),
    };

    Ok((
        OpBuiltPayload::new(
            ctx.payload_id(),
            sealed_block,
            info.total_fees,
            // This must be set to NONE for now because we are doing merge transitions on every flashblock
            // when it should only happen once per block, thus, it returns a confusing state back to op-reth.
            // We can live without this for now because Op syncs up the executed block using new_payload
            // calls, but eventually we would want to return the executed block here.
            None,
        ),
        fb_payload,
        new_bundle,
    ))
}
