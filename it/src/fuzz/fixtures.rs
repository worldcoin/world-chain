// Test utilities for high entropy property-based testing of Block Level Access Lists (BAL)
//
// The bulk of the fixture code lives in `world_chain_test_utils::builder`.  Only
// proptest strategies remain here since `proptest` is not a dependency of the
// `bench` feature.

pub use world_chain_test_utils::builder::*;

use proptest::prelude::*;

/// Strategy for selecting a sender from test signers
pub fn arb_sender() -> impl Strategy<Value = alloy_signer_local::PrivateKeySigner> {
    prop_oneof![
        Just(ALICE.clone()),
        Just(BOB.clone()),
        Just(CHARLIE.clone()),
    ]
}

/// Strategy for generating a single chaos operation
pub fn arb_transaction_op() -> impl Strategy<Value = TxOp> {
    prop_oneof![
        3 => (0usize..1usize).prop_map(|_| TxOp::Transfer { from: ALICE.clone(), to: alloy_primitives::Address::random(), value: alloy_primitives::U256::from(1_000_000_000u64) }),
        2 => (arb_sender(), 2u64..15u64, prop_oneof![Just(ChaosTarget::Direct), Just(ChaosTarget::Proxy),]).prop_map(|(from, n, target)| TxOp::Fib {
            from,
            n,
            target
        }),
        1 => arb_sender().prop_map(|from| TxOp::DeployNewImplementation { from }),
    ]
}

/// Strategy for generating a sequence of chaos operations with proper nonces
pub fn arb_transaction_sequence(max_ops: usize) -> impl Strategy<Value = Vec<(TxOp, u64)>> {
    prop::collection::vec(arb_transaction_op(), 1..=max_ops).prop_map(|ops| {
        let mut nonces: std::collections::HashMap<alloy_primitives::Address, u64> =
            std::collections::HashMap::new();

        ops.into_iter()
            .map(|op| {
                let sender_addr = op.sender().address();
                let nonce = *nonces.entry(sender_addr).or_insert(0);
                nonces.insert(sender_addr, nonce + 1);
                (op, nonce)
            })
            .collect()
    })
}

pub fn arb_execution_payload(
    max_ops_per_flashblock: usize,
) -> impl Strategy<
    Value = (
        world_chain_primitives::primitives::ExecutionPayloadFlashblockDeltaV1,
        Option<
            world_chain_builder::bal_executor::CommittedState<
                reth_optimism_evm::OpRethReceiptBuilder,
            >,
        >,
    ),
> {
    arb_execution_payload_sequence(max_ops_per_flashblock, 1).prop_map(|mut v| v.pop().unwrap())
}

pub fn arb_execution_payload_sequence(
    transactions: usize,
    max_flashblocks: usize,
) -> impl Strategy<
    Value = Vec<(
        world_chain_primitives::primitives::ExecutionPayloadFlashblockDeltaV1,
        Option<
            world_chain_builder::bal_executor::CommittedState<
                reth_optimism_evm::OpRethReceiptBuilder,
            >,
        >,
    )>,
> {
    arb_transaction_sequence(transactions).prop_filter_map(
        "execution must succeed",
        move |sequence: Vec<(TxOp, u64)>| build_chained_payloads(sequence, max_flashblocks).ok(),
    )
}
