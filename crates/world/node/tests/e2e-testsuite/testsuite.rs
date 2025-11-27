use alloy_network::{Ethereum, EthereumWallet, TransactionBuilder, eip2718::Encodable2718};
use alloy_primitives::{B64, b64};
use alloy_rpc_types::TransactionRequest;
use alloy_rpc_types_engine::{ForkchoiceState, PayloadStatus, PayloadStatusEnum};
use ed25519_dalek::SigningKey;
use flashblocks_primitives::{
    flashblocks::{Flashblock, Flashblocks},
    p2p::Authorization,
    primitives::FlashblocksPayloadV1,
};
use futures::{Stream, StreamExt, stream};
use op_alloy_consensus::encode_holocene_extra_data;
use op_alloy_rpc_types_engine::OpExecutionData;
use reth::{
    chainspec::EthChainSpec,
    network::{NetworkSyncUpdater, SyncState},
};
use reth_e2e_test_utils::{testsuite::actions::Action, transaction::TransactionTestContext};
use reth_engine_primitives::ConsensusEngineHandle;
use reth_node_api::{EngineApiMessageVersion, EngineTypes, PayloadAttributes};
use reth_optimism_node::{OpEngineTypes, OpPayloadAttributes, utils::optimism_payload_attributes};
use reth_optimism_payload_builder::payload_id_optimism;
use reth_transaction_pool::TransactionPool;
use revm_primitives::{Address, B256, Bytes, U256, fixed_bytes};
use std::time::Duration;
use tracing::info;
use world_chain_test::utils::account;

use world_chain_node::context::{BasicContext, FlashblocksContext};
use world_chain_test::{
    node::{raw_pbh_bundle_bytes, tx},
    utils::signer,
};

use crate::setup::{
    CHAIN_SPEC, create_test_transaction, encode_eip1559_params, setup, setup_with_tx_peers,
};

#[tokio::test]
async fn test_can_build_pbh_payload() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();
    let (signers, mut nodes, _tasks, _, _) =
        setup::<BasicContext>(1, optimism_payload_attributes).await?;
    let node = &mut nodes[0].node;
    let mut pbh_tx_hashes = vec![];
    let signers = signers.clone();
    for signer in signers.into_iter() {
        let raw_tx =
            raw_pbh_bundle_bytes(signer.into(), 0, 0, U256::ZERO, CHAIN_SPEC.chain_id()).await;
        let pbh_hash = node.rpc.inject_tx(raw_tx.clone()).await?;
        pbh_tx_hashes.push(pbh_hash);
    }

    let payload = node.advance_block().await?;

    assert_eq!(
        payload.block().body().transactions.len(),
        pbh_tx_hashes.len() + 1
    );
    let block_hash = payload.block().hash();
    let block_number = payload.block().number;

    let tip = pbh_tx_hashes[0];
    node.assert_new_block(tip, block_hash, block_number).await?;

    Ok(())
}

#[tokio::test]
async fn test_transaction_pool_ordering() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let (signers, mut nodes, _tasks, _, _) =
        setup::<BasicContext>(1, optimism_payload_attributes).await?;
    let node = &mut nodes[0].node;

    let non_pbh_tx = tx(CHAIN_SPEC.chain.id(), None, 0, Address::default(), 210_000);
    let wallet = signer(0);
    let signer_wallet = EthereumWallet::from(wallet);
    let signed =
        <TransactionRequest as TransactionBuilder<Ethereum>>::build(non_pbh_tx, &signer_wallet)
            .await
            .unwrap();
    let non_pbh_hash = node.rpc.inject_tx(signed.encoded_2718().into()).await?;
    let mut pbh_tx_hashes = vec![];
    let signers = signers.clone();
    for signer in signers.into_iter().skip(1) {
        let raw_tx =
            raw_pbh_bundle_bytes(signer.into(), 0, 0, U256::ZERO, CHAIN_SPEC.chain_id()).await;
        let pbh_hash = node.rpc.inject_tx(raw_tx.clone()).await?;
        pbh_tx_hashes.push(pbh_hash);
    }

    let payload = node.advance_block().await?;

    assert_eq!(
        payload.block().body().transactions.len(),
        pbh_tx_hashes.len() + 2
    );
    // Assert the non-pbh transaction is included in the block last
    assert_eq!(
        *payload.block().body().transactions[payload.block().body().transactions.len() - 2]
            .tx_hash(),
        non_pbh_hash
    );
    let block_hash = payload.block().hash();
    let block_number = payload.block().number;

    let tip = pbh_tx_hashes[0];
    node.assert_new_block(tip, block_hash, block_number).await?;

    Ok(())
}

#[tokio::test]
async fn test_invalidate_dup_tx_and_nullifier() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();
    let (_signers, mut nodes, _tasks, _, _) =
        setup::<BasicContext>(1, optimism_payload_attributes).await?;
    let node = &mut nodes[0].node;
    let signer = 0;
    let raw_tx = raw_pbh_bundle_bytes(signer, 0, 0, U256::ZERO, CHAIN_SPEC.chain_id()).await;
    node.rpc.inject_tx(raw_tx.clone()).await?;
    let dup_pbh_hash_res = node.rpc.inject_tx(raw_tx.clone()).await;
    assert!(dup_pbh_hash_res.is_err());
    Ok(())
}

#[tokio::test]
async fn test_dup_pbh_nonce() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let (_signers, mut nodes, _tasks, _, _) =
        setup::<BasicContext>(1, optimism_payload_attributes).await?;
    let node = &mut nodes[0].node;
    let signer = 0;

    let raw_tx_0 = raw_pbh_bundle_bytes(signer, 0, 0, U256::ZERO, CHAIN_SPEC.chain_id()).await;
    node.rpc.inject_tx(raw_tx_0.clone()).await?;
    let raw_tx_1 = raw_pbh_bundle_bytes(signer, 0, 0, U256::ZERO, CHAIN_SPEC.chain_id()).await;

    // Now that the nullifier has successfully been stored in
    // the `ExecutedPbhNullifierTable`, inserting a new tx with the
    // same pbh_nonce should fail to validate.
    assert!(node.rpc.inject_tx(raw_tx_1.clone()).await.is_err());

    let payload = node.advance_block().await?;

    // One transaction should be successfully validated
    // and included in the block.
    assert_eq!(payload.block().body().transactions.len(), 2);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_flashblocks() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let span_outer = tracing::info_span!("flashblocks_e2e_test");
    let _enter = span_outer.enter();

    // Builder and Follower
    let (_, mut nodes, _tasks, mut flashblocks_env, tx_spammer) =
        setup::<FlashblocksContext>(2, optimism_payload_attributes).await?;

    // Verifier
    let (_, basic_nodes, _tasks, _basic_env, _) =
        setup_with_tx_peers::<BasicContext>(1, optimism_payload_attributes, false, false).await?;

    let basic_worldchain_node = &basic_nodes[0];

    let [builder_node, follower_node] = &mut nodes[..] else {
        unreachable!()
    };

    let builder_context = builder_node.ext_context.clone();
    let follower_context = follower_node.ext_context.clone();

    let basic_beacon_handle = basic_worldchain_node
        .node
        .inner
        .consensus_engine_handle()
        .clone();

    let _spam_task = tokio::spawn(async move {
        tx_spammer.spawn(10).await;
    });

    let block_hash = builder_node.node.block_hash(0);
    let block_hash_basic = basic_worldchain_node.node.block_hash(0);

    assert_eq!(block_hash, block_hash_basic);
    let authorization_generator = crate::setup::create_authorization_generator(
        block_hash,
        builder_context
            .flashblocks_handle
            .builder_sk()
            .unwrap()
            .verifying_key()
            .clone(),
    );

    let timestamp = crate::setup::current_timestamp();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let eip1559_params = crate::setup::encode_eip1559_params(
        builder_node.node.inner.chain_spec().as_ref(),
        timestamp,
    )?;
    let attributes = crate::setup::build_payload_attributes(
        timestamp,
        eip1559_params,
        Some(vec![crate::setup::TX_SET_L1_BLOCK.clone()]),
    );

    let mut flashblocks_action = crate::actions::AssertMineBlock::new(
        0,
        vec![],
        Some(B256::ZERO),
        attributes.clone(),
        authorization_generator.clone(),
        std::time::Duration::from_secs(2),
        true,
        true,
        tx.clone(),
    )
    .await;

    flashblocks_action.execute(&mut flashblocks_env).await?;

    let execution_payload_envelope = rx.recv().await.expect("should receive payload");
    let expected_block_hash = execution_payload_envelope
        .execution_payload
        .payload_inner
        .payload_inner
        .block_hash;

    // Create a stream that validates each intermediate flashblock against the beacon node
    let validation_stream = crate::setup::flashblocks_validator_stream(
        follower_context.flashblocks_handle.flashblock_stream(),
        basic_beacon_handle.clone(),
        expected_block_hash,
        basic_worldchain_node.node.inner.chain_spec().clone(),
    );

    futures::pin_mut!(validation_stream);

    // Consume the validation stream, asserting each flashblock is valid
    let mut validated_count = 0u64;
    while let Some(validated) = validation_stream.next().await {
        validated_count += 1;

        // Assert the payload is valid
        assert!(
            matches!(
                validated.status,
                PayloadStatus {
                    status: PayloadStatusEnum::Valid,
                    ..
                }
            ),
            "Intermediate flashblock {} should be valid, got: {:?}",
            validated.index,
            validated.status
        );

        info!(
            target: "flashblocks",
            index = %validated.index,
            validated_count = %validated_count,
            is_final = %validated.is_final,
            block_hash = ?validated.execution_data.block_hash(),
            "Validated intermediate flashblock"
        );

        if validated.is_final {
            info!(
                target: "flashblocks",
                validated_count = %validated_count,
                "All flashblocks validated successfully, reached final block"
            );
            break;
        }
    }

    assert!(
        validated_count > 0,
        "Should have validated at least one flashblock"
    );

    info!(
        target: "flashblocks",
        expected_block_hash = ?expected_block_hash,
        validated_count = %validated_count,
        "Flashblock validation complete",
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_eth_api_receipt() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let (_, nodes, _tasks, mut env, _spammer) =
        setup::<FlashblocksContext>(3, optimism_payload_attributes).await?;

    let ext_context = nodes[0].ext_context.clone();
    let block_hash = nodes[0].node.block_hash(0);

    let authorization_generator = crate::setup::create_authorization_generator(
        block_hash,
        ext_context
            .flashblocks_handle
            .builder_sk()
            .unwrap()
            .verifying_key()
            .clone(),
    );

    let timestamp = crate::setup::current_timestamp();
    let (sender, mut block_rx) = tokio::sync::mpsc::channel(1);
    let eip1559_params =
        crate::setup::encode_eip1559_params(nodes[0].node.inner.chain_spec().as_ref(), timestamp)?;

    // Compose a Mine Block action with an eth_getTransactionReceipt action
    let attributes = crate::setup::build_payload_attributes(
        timestamp,
        eip1559_params,
        Some(vec![crate::setup::TX_SET_L1_BLOCK.clone()]),
    );

    let mock_tx =
        TransactionTestContext::transfer_tx(nodes[0].node.inner.chain_spec().chain_id(), signer(0))
            .await;

    let raw_tx: Bytes = mock_tx.encoded_2718().into();

    nodes[0].node.rpc.inject_tx(raw_tx.clone()).await?;

    let mut hashes = vec![];
    for i in 1..20 {
        let mock_tx = TransactionTestContext::transfer_tx(
            nodes[0].node.inner.chain_spec().chain_id(),
            signer(i),
        )
        .await;

        let raw_tx: Bytes = mock_tx.encoded_2718().into();

        let hash = nodes[0].node.rpc.inject_tx(raw_tx.clone()).await?;
        hashes.push(hash);
    }

    let mine_block = crate::actions::AssertMineBlock::new(
        0,
        vec![],
        None,
        attributes,
        authorization_generator,
        std::time::Duration::from_millis(2000),
        true,
        true,
        sender,
    )
    .await;

    let (tx, mut rx) = tokio::sync::mpsc::channel(1);

    let transaction_receipt =
        crate::actions::EthGetTransactionReceipt::new(hashes, vec![0, 1, 2], 240, tx);

    let mut action = crate::actions::EthApiAction::new(mine_block, transaction_receipt);
    action.execute(&mut env).await?;

    let mined_block = block_rx.recv().await.expect("should receive mined block");
    let receipts: Vec<Vec<_>> = rx.recv().await.expect("should receive receipts");

    info!("Receipts: {:?}", receipts);

    assert_eq!(
        receipts.len(),
        3,
        "Should receive receipts from all 3 nodes"
    );

    for (idx, receipt_opt) in receipts.iter().enumerate() {
        assert!(
            receipt_opt.iter().all(|r| r.is_some()),
            "Node {} should return a receipt",
            idx
        );
    }

    let receipts: Vec<Vec<_>> = receipts
        .into_iter()
        .map(|r| r.iter().map(|r| r.clone().unwrap()).collect())
        .collect();

    let first_node_receipts = &receipts[0];

    for (_, receipts) in receipts.iter().enumerate().skip(1) {
        assert_eq!(
            receipts.len() + 2,
            mined_block
                .execution_payload
                .payload_inner
                .payload_inner
                .transactions
                .len(),
            "Receipt count should match transaction count"
        );

        for (tx_idx, receipt) in receipts.iter().enumerate() {
            let block_hash = mined_block
                .execution_payload
                .payload_inner
                .payload_inner
                .block_hash;

            assert_eq!(
                receipt.inner.block_hash,
                Some(block_hash),
                "Receipt block hash should match mined block hash"
            );

            assert_eq!(
                receipt, &first_node_receipts[tx_idx],
                "Receipts should match across nodes"
            );
        }

        assert_eq!(receipts.len(), first_node_receipts.len())
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_eth_api_call() -> eyre::Result<()> {
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let (_, nodes, _tasks, mut env, _) =
        setup::<FlashblocksContext>(3, optimism_payload_attributes).await?;

    let ext_context = nodes[0].ext_context.clone();
    let block_hash = nodes[0].node.block_hash(0);

    let authorization_generator = crate::setup::create_authorization_generator(
        block_hash,
        ext_context
            .flashblocks_handle
            .builder_sk()
            .unwrap()
            .verifying_key()
            .clone(),
    );

    let timestamp = crate::setup::current_timestamp();
    let (sender, _) = tokio::sync::mpsc::channel(1);

    // 200ms backoff should be enough time to fetch the pending receipt
    let mut mock_tx: TransactionRequest = tx(
        nodes[0].node.inner.chain_spec().chain_id(),
        None,
        0,
        Address::ZERO,
        21_000,
    )
    .value(U256::from(100_000_000_000_000_000u64))
    .from(account(0));

    let wallet = EthereumWallet::from(signer(0));
    let raw_tx = crate::setup::sign_transaction(mock_tx.clone(), &wallet).await;

    let attributes = crate::setup::build_payload_attributes(
        timestamp,
        b64!("0000000800000008"),
        Some(vec![raw_tx.clone()]),
    );

    let mine_block = crate::actions::AssertMineBlock::new(
        0,
        vec![],
        Some(B256::ZERO),
        attributes,
        authorization_generator,
        std::time::Duration::from_millis(2000),
        true,
        false,
        sender,
    )
    .await;

    let (tx, mut rx) = tokio::sync::mpsc::channel(1);

    mock_tx.value = Some(U256::from(1));
    mock_tx.nonce = None;

    let eth_call = crate::actions::EthCall::new(mock_tx, vec![0, 1, 2], 200, tx);

    let mut action = crate::actions::EthApiAction::new(mine_block, eth_call);

    action.execute(&mut env).await?;

    let call_results = rx.recv().await.expect("should receive call results");

    for call_result in call_results {
        assert_eq!(call_result.as_ref(), b"");
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_op_api_supported_capabilities_call() -> eyre::Result<()> {
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let (_, _nodes, _tasks, mut env, _) =
        setup::<FlashblocksContext>(1, optimism_payload_attributes).await?;

    let (tx, mut rx) = tokio::sync::mpsc::channel(1);

    let mut action = crate::actions::SupportedCapabilitiesCall::new(tx);

    action.execute(&mut env).await?;

    let call_results = rx.recv().await.expect("should receive call results");

    assert_eq!(call_results, vec!["flashblocksv1".to_string()]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_eth_block_by_hash_pending() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let (_, nodes, _tasks, mut env, spammer) =
        setup::<FlashblocksContext>(2, optimism_payload_attributes).await?;

    tokio::spawn(async move { spammer.spawn(10) });

    let ext_context = nodes[0].ext_context.clone();
    let block_hash = nodes[0].node.block_hash(0);

    let authorization_generator = crate::setup::create_authorization_generator(
        block_hash,
        ext_context
            .flashblocks_handle
            .builder_sk()
            .unwrap()
            .verifying_key()
            .clone(),
    );

    let (sender, _) = tokio::sync::mpsc::channel(1);
    let timestamp = crate::setup::current_timestamp();
    let eip1559_params =
        encode_eip1559_params(nodes[0].node.inner.chain_spec().as_ref(), timestamp)?;

    // Note: Using a fixed timestamp for deterministic block hash in this test
    let attributes = OpPayloadAttributes {
        payload_attributes: alloy_rpc_types_engine::PayloadAttributes {
            timestamp: 1756929279,
            prev_randao: B256::ZERO,
            suggested_fee_recipient: Address::ZERO,
            withdrawals: Some(vec![]),
            parent_beacon_block_root: Some(B256::ZERO),
        },
        transactions: Some(vec![]),
        no_tx_pool: Some(false),
        eip_1559_params: Some(eip1559_params),
        gas_limit: Some(30_000_000),
        min_base_fee: None,
    };

    let mine_block = crate::actions::AssertMineBlock::new(
        0,
        vec![],
        Some(B256::ZERO),
        attributes,
        authorization_generator,
        std::time::Duration::from_millis(2000),
        true,
        false,
        sender,
    )
    .await;

    let (tx, mut rx) = tokio::sync::mpsc::channel(1);

    let pending_hash =
        fixed_bytes!("0x080d3c4547e6f4133dfe28bfd35511e16add1778a8904dd6f65a30c79803c635");

    let eth_block_by_hash = crate::actions::EthGetBlockByHash::new(pending_hash, vec![0], 230, tx);
    let mut action = crate::actions::EthApiAction::new(mine_block, eth_block_by_hash);

    action.execute(&mut env).await?;

    // ensure the pre-confirmed block exists on the path of the pending tag
    let mut blocks = rx.recv().await.expect("should receive block");
    blocks.pop().expect("should have one block").unwrap();

    Ok(())
}

// Transaction Propagation Tests

/// Test default transaction propagation behavior without tx_peers configuration
///
/// Verifies that without tx_peers configuration, transactions propagate to ALL connected peers
/// using Reth's default TransactionPropagationKind::All policy.
#[tokio::test]
async fn test_default_propagation_policy() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    // Spin up 3 nodes WITHOUT tx_peers configuration
    let (_, mut nodes, _tasks, _, _) =
        setup::<FlashblocksContext>(3, optimism_payload_attributes).await?;

    let [node_0_ctx, node_1_ctx, node_2_ctx] = &mut nodes[..] else {
        unreachable!()
    };

    // Set nodes to Idle state to enable transaction propagation
    node_0_ctx
        .node
        .inner
        .network
        .update_sync_state(SyncState::Idle);
    node_1_ctx
        .node
        .inner
        .network
        .update_sync_state(SyncState::Idle);
    node_2_ctx
        .node
        .inner
        .network
        .update_sync_state(SyncState::Idle);

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Create and inject transaction into Node 0
    let (raw_tx, tx_hash) = crate::setup::create_test_transaction(0, 0).await;

    let result = node_0_ctx.node.rpc.inject_tx(raw_tx).await;
    assert!(result.is_ok(), "Transaction should be accepted by Node 0");

    tokio::time::sleep(Duration::from_secs(5)).await;

    assert!(
        node_0_ctx.node.inner.pool().contains(&tx_hash),
        "Transaction should be in Node 0's pool"
    );

    assert!(
        node_1_ctx.node.inner.pool().contains(&tx_hash),
        "Transaction SHOULD propagate to Node 1 with default policy (TransactionPropagationKind::All)"
    );

    assert!(
        node_2_ctx.node.inner.pool().contains(&tx_hash),
        "Transaction SHOULD propagate to Node 2 with default policy (TransactionPropagationKind::All)"
    );

    Ok(())
}

/// Test selective transaction propagation with tx_peers configuration
///
/// Verifies that with tx_peers configuration, transactions only propagate to whitelisted peers
/// using WorldChainTransactionPropagationPolicy.
///
/// Setup:
/// - Node 0: no tx_peers (default propagation)
/// - Node 1: tx_peers = [Node 0] (only propagates to Node 0)
/// - Node 2: tx_peers = [Node 0, Node 1] (propagates to both)
///
/// Test Part 1:
/// - Inject tx into Node 1 -> should propagate to Node 0 only
/// - Node 2 should NOT receive it (not in Node 1's whitelist)
///
/// Test Part 2:
/// - Inject tx into Node 2 -> should propagate to both Node 0 and Node 1
/// - Verifies multi-peer whitelist works correctly
#[tokio::test]
async fn test_selective_propagation_policy() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    // We disconnect Node 0 from Node 2 to prevent multi-hop forwarding in Part 1
    let (_, mut nodes, _tasks, _, _) =
        setup_with_tx_peers::<FlashblocksContext>(3, optimism_payload_attributes, true, false)
            .await?;

    let [node_0_ctx, node_1_ctx, node_2_ctx] = &mut nodes[..] else {
        unreachable!()
    };

    let node_0_peer_id = node_0_ctx.node.network.record().id;
    let node_1_peer_id = node_1_ctx.node.network.record().id;
    let node_2_peer_id = node_2_ctx.node.network.record().id;

    // Set nodes to Idle state to enable transaction propagation
    use reth::network::Peers;
    node_0_ctx
        .node
        .inner
        .network
        .update_sync_state(SyncState::Idle);
    node_1_ctx
        .node
        .inner
        .network
        .update_sync_state(SyncState::Idle);
    node_2_ctx
        .node
        .inner
        .network
        .update_sync_state(SyncState::Idle);

    // Disconnect Node 0 from Node 2 to prevent multi-hop forwarding
    node_0_ctx
        .node
        .inner
        .network
        .disconnect_peer(node_2_peer_id);
    node_2_ctx
        .node
        .inner
        .network
        .disconnect_peer(node_0_peer_id);

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Create and inject transaction into Node 1 (which has tx_peers = [Node 0 only])
    let (raw_tx, tx_hash) = crate::setup::create_test_transaction(0, 0).await;

    let result = node_1_ctx.node.rpc.inject_tx(raw_tx).await;
    assert!(result.is_ok(), "Transaction should be accepted by Node 1");

    tokio::time::sleep(Duration::from_secs(3)).await;

    assert!(
        node_1_ctx.node.inner.pool().contains(&tx_hash),
        "Transaction should be in Node 1's pool"
    );

    assert!(
        node_0_ctx.node.inner.pool().contains(&tx_hash),
        "Transaction SHOULD propagate to Node 0 (whitelisted in Node 1's tx_peers)"
    );

    assert!(
        !node_2_ctx.node.inner.pool().contains(&tx_hash),
        "Transaction should NOT propagate to Node 2 (NOT in Node 1's tx_peers whitelist)"
    );

    // Part 2: Test Node 2 -> Node 0 and Node 1
    // Disconnect node 0 and node 1
    node_0_ctx
        .node
        .inner
        .network
        .disconnect_peer(node_1_peer_id);
    node_1_ctx
        .node
        .inner
        .network
        .disconnect_peer(node_0_peer_id);

    tokio::time::sleep(Duration::from_secs(3)).await;

    // Reconnect Node 0 and Node 2
    let node_0_addr = node_0_ctx.node.network.record().tcp_addr();
    node_2_ctx
        .node
        .inner
        .network
        .add_peer(node_0_peer_id, node_0_addr);

    tokio::time::sleep(Duration::from_secs(3)).await;

    // Create a new transaction and inject into Node 2
    // Node 2 has tx_peers = [Node 0, Node 1], so it should propagate to both
    let (raw_tx_2, tx_hash_2) = crate::setup::create_test_transaction(1, 0).await;

    // Inject transaction into Node 2
    let result = node_2_ctx.node.rpc.inject_tx(raw_tx_2).await;
    assert!(result.is_ok(), "Transaction should be accepted by Node 2");

    tokio::time::sleep(Duration::from_secs(2)).await;

    assert!(
        node_2_ctx.node.inner.pool().contains(&tx_hash_2),
        "Transaction should be in Node 2's pool"
    );

    assert!(
        node_0_ctx.node.inner.pool().contains(&tx_hash_2),
        "Transaction SHOULD propagate to Node 0 (whitelisted in Node 2's tx_peers)"
    );

    assert!(
        node_1_ctx.node.inner.pool().contains(&tx_hash_2),
        "Transaction SHOULD propagate to Node 1 (whitelisted in Node 2's tx_peers)"
    );

    Ok(())
}

/// Test that transactions do NOT propagate when gossip is completely disabled
///
/// Verifies that when --rollup.disable-tx-pool-gossip is set, transactions do NOT
/// propagate to ANY peers, regardless of tx_peers configuration. This flag completely
/// disables transaction gossip and takes precedence over tx_peers whitelist.
///
/// Setup:
/// - Node 0: gossip disabled, no tx_peers
/// - Node 1: gossip disabled, tx_peers = [Node 0]
/// - Node 2: gossip disabled, tx_peers = [Node 0, Node 1]
///
/// Test:
/// - Inject tx into Node 0 -> should NOT propagate to any node
/// - Inject tx into Node 1 -> should NOT propagate to any node (even though Node 0 is whitelisted)
/// - Verifies that disable_txpool_gossip takes precedence over tx_peers
#[tokio::test]
async fn test_gossip_disabled_no_propagation() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let (_, mut nodes, _tasks, _, _) =
        setup_with_tx_peers::<FlashblocksContext>(3, optimism_payload_attributes, true, true)
            .await?;

    let [node_0_ctx, node_1_ctx, node_2_ctx] = &mut nodes[..] else {
        unreachable!()
    };

    // Set nodes to Idle state to enable transaction propagation
    node_0_ctx
        .node
        .inner
        .network
        .update_sync_state(SyncState::Idle);
    node_1_ctx
        .node
        .inner
        .network
        .update_sync_state(SyncState::Idle);
    node_2_ctx
        .node
        .inner
        .network
        .update_sync_state(SyncState::Idle);

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Inject transaction into Node 1 (which has tx_peers = [Node 0])
    // Even with tx_peers configured, gossip disabled should prevent propagation
    let (raw_tx, tx_hash) = create_test_transaction(1, 0).await;

    let result = node_1_ctx.node.rpc.inject_tx(raw_tx).await;
    assert!(result.is_ok(), "Transaction should be accepted by Node 1");

    // Wait for potential propagation
    tokio::time::sleep(Duration::from_secs(3)).await;

    assert!(
        node_1_ctx.node.inner.pool().contains(&tx_hash),
        "Transaction should be in Node 1's pool"
    );

    assert!(
        !node_0_ctx.node.inner.pool().contains(&tx_hash),
        "Transaction should NOT propagate to Node 0 (gossip disabled takes precedence over tx_peers)"
    );

    assert!(
        !node_2_ctx.node.inner.pool().contains(&tx_hash),
        "Transaction should NOT propagate to Node 2 (gossip disabled)"
    );

    Ok(())
}
