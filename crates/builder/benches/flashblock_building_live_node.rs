use std::sync::Arc;

use alloy_primitives::{Address, B256, Bytes};
use alloy_rpc_types_engine::PayloadAttributes as RpcPayloadAttributes;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use reth_basic_payload_builder::{BuildArguments, BuildOutcome, PayloadConfig};
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{
    OpPayloadAttributes, payload::OpPayloadAttrs, utils::optimism_payload_attributes,
};
use reth_optimism_payload_builder::config::OpBuilderConfig;
use reth_payload_primitives::PayloadAttributes as _;
use reth_provider::{StateProvider, StateProviderFactory};
use world_chain_builder::{
    WorldChainPayloadBuilderCtxBuilder, payload_builder::FlashblocksPayloadBuilder,
    traits::payload_builder::FlashblockPayloadBuilder,
};
use world_chain_node::context::WorldChainDefaultContext;
use world_chain_test_utils::{
    PBH_DEV_ENTRYPOINT, PBH_DEV_SIGNATURE_AGGREGATOR,
    builder::{
        CHAIN_SPEC, build_flashblock_fixture_eth_transfers_with_provider,
        build_flashblock_fixture_fib_with_provider,
        build_flashblock_fixture_world_id_like_bn254_with_provider,
    },
    e2e_harness::setup::{
        TX_SET_L1_BLOCK, encode_eip1559_params, setup_with_block_uncompressed_size_limit,
    },
    utils::signer,
};

const TOTAL_TX_COUNTS: [usize; 3] = [50, 500, 1000];
const WORLD_ID_TOTAL_TX_COUNTS: [usize; 3] = [10, 25, 50];

fn deterministic_payload_attributes(
    timestamp: u64,
    transactions: Vec<Bytes>,
) -> OpPayloadAttributes {
    let eip1559_params = encode_eip1559_params(CHAIN_SPEC.as_ref(), timestamp)
        .expect("failed to encode eip1559 params");

    OpPayloadAttributes {
        payload_attributes: RpcPayloadAttributes {
            timestamp,
            prev_randao: B256::ZERO,
            suggested_fee_recipient: Address::ZERO,
            withdrawals: Some(vec![]),
            parent_beacon_block_root: Some(B256::ZERO),
            slot_number: None,
        },
        transactions: Some(transactions),
        no_tx_pool: Some(true),
        eip_1559_params: Some(eip1559_params),
        gas_limit: Some(CHAIN_SPEC.genesis_header().gas_limit),
        min_base_fee: CHAIN_SPEC
            .is_jovian_active_at_timestamp(timestamp)
            .then_some(0),
    }
}

fn build_payload_transactions<F>(
    state_provider: &dyn StateProvider,
    total_tx_count: usize,
    build_flashblock: F,
) -> Vec<Bytes>
where
    F: Fn(
        &dyn StateProvider,
        usize,
        bool,
    ) -> world_chain_primitives::primitives::FlashblocksPayloadV1,
{
    let user_tx_count = total_tx_count
        .checked_sub(1)
        .expect("flashblock building bench requires room for the L1 attributes tx");

    let flashblock = build_flashblock(state_provider, user_tx_count, false);
    let mut transactions = Vec::with_capacity(total_tx_count);
    transactions.push(TX_SET_L1_BLOCK.clone());
    transactions.extend(flashblock.diff.transactions);
    assert_eq!(transactions.len(), total_tx_count);
    transactions
}

fn build_live_payload_builder<Pool, Client>(
    pool: Pool,
    client: Client,
    evm_config: reth_optimism_node::OpEvmConfig,
    bal_enabled: bool,
) -> FlashblocksPayloadBuilder<Pool, Client, WorldChainPayloadBuilderCtxBuilder, ()>
where
    Pool: Clone,
    Client: Clone,
{
    FlashblocksPayloadBuilder {
        evm_config,
        pool,
        client,
        builder_config: OpBuilderConfig::default(),
        bal_enabled,
        best_transactions: (),
        ctx_builder: WorldChainPayloadBuilderCtxBuilder {
            verified_blockspace_capacity: 70,
            pbh_entry_point: PBH_DEV_ENTRYPOINT,
            pbh_signature_aggregator: PBH_DEV_SIGNATURE_AGGREGATOR,
            builder_private_key: signer(6),
            block_uncompressed_size_limit: None,
        },
        metrics: Default::default(),
    }
}

fn bench_build_flashblock_case<F>(
    c: &mut Criterion,
    group_name: &str,
    bal_enabled: bool,
    tx_counts: &[usize],
    build_transactions: F,
) where
    F: Fn(&dyn StateProvider, usize) -> Vec<Bytes>,
{
    let rt = tokio::runtime::Runtime::new().expect("failed to build tokio runtime");
    let mut group = c.benchmark_group(group_name);
    group.sample_size(20);

    for &tx_count in tx_counts {
        let (_, nodes, _, _, _) = rt
            .block_on(setup_with_block_uncompressed_size_limit::<
                WorldChainDefaultContext,
            >(
                1,
                optimism_payload_attributes,
                true,
                None,
                CHAIN_SPEC.clone(),
            ))
            .unwrap();
        let node = &nodes[0];
        let provider = node.node.inner.provider.clone();
        let latest = provider
            .latest()
            .expect("failed to obtain latest state provider from node");
        let transactions = build_transactions(latest.as_ref(), tx_count);
        let parent_header = node.node.inner.chain_spec().sealed_genesis_header();
        let timestamp = parent_header.timestamp + 2; // 2s block time
        let rpc_attributes = deterministic_payload_attributes(timestamp, transactions);
        let attributes = OpPayloadAttrs::from(rpc_attributes);
        let payload_id = attributes.payload_id(&parent_header.hash());
        let config = PayloadConfig::new(Arc::new(parent_header.clone()), attributes, payload_id);
        let builder = build_live_payload_builder(
            node.node.inner.pool.clone(),
            provider,
            node.node.inner.evm_config.clone(),
            bal_enabled,
        );

        group.bench_with_input(BenchmarkId::new("txs", tx_count), &tx_count, |b, &_n| {
            b.iter(|| {
                let args = BuildArguments {
                    config: config.clone(),
                    cached_reads: Default::default(),
                    cancel: Default::default(),
                    best_payload: None,
                    execution_cache: None,
                    trie_handle: None,
                };

                let (outcome, access_list) = builder
                    .try_build_with_precommit(args, None)
                    .expect("flashblock build failed");

                let payload = match outcome {
                    BuildOutcome::Freeze(payload) => payload,
                    other => panic!("expected a frozen payload, got {other:?}"),
                };

                assert_eq!(payload.block().body().transactions().count(), tx_count);
                assert_eq!(access_list.is_some(), bal_enabled);
            });
        });
    }

    group.finish();
}

fn bench_build_flashblock_eth_transfers_live(c: &mut Criterion) {
    bench_build_flashblock_case(
        c,
        "build_flashblock_eth_transfers_live",
        false,
        &TOTAL_TX_COUNTS,
        |state_provider: &dyn StateProvider, tx_count| {
            build_payload_transactions(state_provider, tx_count, |provider, user_tx_count, bal| {
                build_flashblock_fixture_eth_transfers_with_provider(provider, user_tx_count, bal)
            })
        },
    );
}

fn bench_build_flashblock_eth_transfers_with_bal_live(c: &mut Criterion) {
    bench_build_flashblock_case(
        c,
        "build_flashblock_eth_transfers_with_bal_live",
        true,
        &TOTAL_TX_COUNTS,
        |state_provider: &dyn StateProvider, tx_count| {
            build_payload_transactions(state_provider, tx_count, |provider, user_tx_count, bal| {
                build_flashblock_fixture_eth_transfers_with_provider(provider, user_tx_count, bal)
            })
        },
    );
}

fn bench_build_flashblock_fib_live(c: &mut Criterion) {
    bench_build_flashblock_case(
        c,
        "build_flashblock_fib_live",
        false,
        &TOTAL_TX_COUNTS,
        |state_provider: &dyn StateProvider, tx_count| {
            build_payload_transactions(state_provider, tx_count, |provider, user_tx_count, bal| {
                build_flashblock_fixture_fib_with_provider(provider, user_tx_count, bal)
            })
        },
    );
}

fn bench_build_flashblock_fib_with_bal_live(c: &mut Criterion) {
    bench_build_flashblock_case(
        c,
        "build_flashblock_fib_with_bal_live",
        true,
        &TOTAL_TX_COUNTS,
        |state_provider: &dyn StateProvider, tx_count| {
            build_payload_transactions(state_provider, tx_count, |provider, user_tx_count, bal| {
                build_flashblock_fixture_fib_with_provider(provider, user_tx_count, bal)
            })
        },
    );
}

fn bench_build_flashblock_world_id_like_bn254_live(c: &mut Criterion) {
    bench_build_flashblock_case(
        c,
        "build_flashblock_world_id_like_bn254_live",
        false,
        &WORLD_ID_TOTAL_TX_COUNTS,
        |state_provider: &dyn StateProvider, tx_count| {
            build_payload_transactions(state_provider, tx_count, |provider, user_tx_count, bal| {
                build_flashblock_fixture_world_id_like_bn254_with_provider(
                    provider,
                    user_tx_count,
                    bal,
                )
            })
        },
    );
}

fn bench_build_flashblock_world_id_like_bn254_with_bal_live(c: &mut Criterion) {
    bench_build_flashblock_case(
        c,
        "build_flashblock_world_id_like_bn254_with_bal_live",
        true,
        &WORLD_ID_TOTAL_TX_COUNTS,
        |state_provider: &dyn StateProvider, tx_count| {
            build_payload_transactions(state_provider, tx_count, |provider, user_tx_count, bal| {
                build_flashblock_fixture_world_id_like_bn254_with_provider(
                    provider,
                    user_tx_count,
                    bal,
                )
            })
        },
    );
}

criterion_group!(
    benches,
    bench_build_flashblock_eth_transfers_live,
    bench_build_flashblock_eth_transfers_with_bal_live,
    bench_build_flashblock_fib_live,
    bench_build_flashblock_fib_with_bal_live,
    bench_build_flashblock_world_id_like_bn254_live,
    bench_build_flashblock_world_id_like_bn254_with_bal_live,
);
criterion_main!(benches);
