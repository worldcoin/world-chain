use crate::{
    actions::{
        ActionSequence, BlockProductionState, DynamicMineBlock, DynamicValidateFlashblocks,
        LogBlockComplete, QueryTxReceipts, QueryValidatedBlocks, ResetState, Sleep,
    },
    setup::{TX_SET_L1_BLOCK, build_payload_attributes},
};
use alloy_network::{Ethereum, EthereumWallet, TransactionBuilder, eip2718::Encodable2718};
use alloy_primitives::b64;
use alloy_rpc_types::TransactionRequest;
use alloy_rpc_types_engine::PayloadStatusEnum;
use eyre::eyre::eyre;
use futures::future::Either;
use reth::{
    chainspec::EthChainSpec,
    network::{NetworkSyncUpdater, SyncState},
};
use reth_e2e_test_utils::testsuite::actions::Action;
use reth_optimism_node::utils::optimism_payload_attributes;
use reth_transaction_pool::TransactionPool;
use revm_primitives::{Address, U256};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tracing::info;
use world_chain_test::{
    node::{raw_pbh_bundle_bytes, tx},
    utils::{account, signer},
};

use crate::setup::{
    CHAIN_SPEC, create_test_transaction, encode_eip1559_params, setup, setup_with_tx_peers,
};

#[tokio::test]
async fn test_can_build_pbh_payload() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();
    let (signers, mut nodes, _tasks, _, _) =
        setup(1, optimism_payload_attributes, false).await?;
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
        setup(1, optimism_payload_attributes, false).await?;
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
        setup(1, optimism_payload_attributes, false).await?;
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
        setup(1, optimism_payload_attributes, false).await?;
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

    const TRANSACTIONS_PER_FLASHBLOCK: u64 = 20;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Builder and Follower
    let (_, mut nodes, _tasks, mut flashblocks_env, tx_spammer) =
        setup(2, optimism_payload_attributes, true).await?;

    // Verifier
    let (_, basic_nodes, _tasks, mut basic_env, _) = setup_with_tx_peers(
        1,
        optimism_payload_attributes,
        false,
        false,
        true,
    )
    .await?;

    let basic_worldchain_node = &basic_nodes[0];

    let [builder_node, _follower_node] = &mut nodes[..] else {
        unreachable!()
    };

    let builder_context = builder_node.ext_context.clone();
    let rpc_url = builder_node.node.rpc_url();

    tx_spammer.spawn(TRANSACTIONS_PER_FLASHBLOCK, rpc_url);

    let block_hash = builder_node.node.block_hash(0);
    let block_hash_basic = basic_worldchain_node.node.block_hash(0);

    assert_eq!(block_hash, block_hash_basic);

    let authorization_generator = crate::setup::create_authorization_generator(
        block_hash,
        builder_context
            .clone()
            .unwrap()
            .flashblocks_handle
            .builder_sk()
            .unwrap()
            .verifying_key(),
    );

    let timestamp = crate::setup::current_timestamp();

    let eip1559_params = crate::setup::encode_eip1559_params(
        builder_node.node.inner.chain_spec().as_ref(),
        timestamp,
    )?;

    let attributes = crate::setup::build_payload_attributes(
        timestamp,
        eip1559_params,
        Some(vec![crate::setup::TX_SET_L1_BLOCK.clone()]),
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel(1);

    let _tx = tx.clone();

    let mine_block = crate::actions::AssertMineBlock::new(
        0,
        None,
        attributes,
        authorization_generator,
        std::time::Duration::from_millis(3000),
        true,
        tx,
    )
    .await;

    let cannon_flashblocks_stream = Box::pin(
        builder_context
            .as_ref()
            .unwrap()
            .flashblocks_handle
            .flashblock_stream(),
    );

    let validation_stream = crate::actions::FlashblocksValidatonStream {
        beacon_engine_handles: vec![
            basic_worldchain_node
                .node
                .inner
                .consensus_engine_handle()
                .clone()
                .into(),
        ],
        flashblocks_stream: cannon_flashblocks_stream,
        validation_hook: Some(Arc::new(move |status: PayloadStatusEnum| {
            info!(target: "test", "Flashblock validated with status: {:?}", status);
            assert_eq!(status, PayloadStatusEnum::Valid);
            Ok(())
        })),
        chain_spec: basic_worldchain_node.node.inner.chain_spec().clone(),
    };

    // Run mining and validation concurrently - validation must be listening
    // while mining produces flashblocks, otherwise they'll be missed
    tokio::spawn(async move {
        let mut mine_action = mine_block;
        mine_action.execute(&mut flashblocks_env).await
    });

    tokio::spawn(async move {
        let mut validation_action = validation_stream;
        validation_action.execute(&mut basic_env).await
    });

    // Wait for mining to complete
    rx.recv()
        .await
        .ok_or(eyre!("failed to receive mined block"))?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_eth_api_receipt() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let (_, nodes, _tasks, mut env, _spammer) =
        setup(3, optimism_payload_attributes, true).await?;

    let ext_context = nodes[0].ext_context.clone();
    let block_hash = nodes[0].node.block_hash(0);

    let authorization_generator = crate::setup::create_authorization_generator(
        block_hash,
        ext_context
            .unwrap()
            .flashblocks_handle
            .builder_sk()
            .unwrap()
            .verifying_key(),
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

    let cannon_flashblocks_stream = nodes[0]
        .ext_context
        .clone()
        .unwrap()
        .flashblocks_handle
        .flashblock_stream();

    let mine_block = crate::actions::AssertMineBlock::new(
        0,
        None,
        attributes,
        authorization_generator,
        std::time::Duration::from_millis(2000),
        true,
        sender,
    )
    .await;

    let transaction_receipt =
        crate::actions::GetReceipts::new(vec![0, 1, 2], cannon_flashblocks_stream).on_receipts(
            move |receipts| {
                for receipts in receipts {
                    crate::actions::assert::all_some(&receipts, "transaction receipt")?;
                }

                Ok(())
            },
        );

    let mut action = crate::actions::EthApiAction::new(mine_block, transaction_receipt);

    let fut = async { action.execute(&mut env).await };

    futures::future::try_select(
        Box::pin(fut),
        Box::pin(async {
            block_rx
                .recv()
                .await
                .ok_or(eyre!("failed to fetch envelope"))
        }),
    )
    .await
    .map_err(|e| eyre!("{}", e.factor_first().0))?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_eth_api_call() -> eyre::Result<()> {
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let (_, nodes, _tasks, mut env, _) =
        setup(3, optimism_payload_attributes, true).await?;

    let ext_context = nodes[0].ext_context.clone();
    let block_hash = nodes[0].node.block_hash(0);

    let authorization_generator = crate::setup::create_authorization_generator(
        block_hash,
        ext_context
            .unwrap()
            .flashblocks_handle
            .builder_sk()
            .unwrap()
            .verifying_key(),
    );

    let timestamp = crate::setup::current_timestamp();
    let (sender, _rx) = tokio::sync::mpsc::channel(1);

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
        None,
        attributes,
        authorization_generator,
        std::time::Duration::from_millis(2000),
        true,
        sender,
    )
    .await;

    let (tx, mut rx) = tokio::sync::mpsc::channel(3);

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
        setup(1, optimism_payload_attributes, true).await?;

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
        setup(2, optimism_payload_attributes, true).await?;

    let ext_context = nodes[0].ext_context.clone();
    let block_hash = nodes[0].node.block_hash(0);

    let authorization_generator = crate::setup::create_authorization_generator(
        block_hash,
        ext_context
            .unwrap()
            .flashblocks_handle
            .builder_sk()
            .unwrap()
            .verifying_key(),
    );

    spammer.spawn(20, nodes[0].node.rpc_url());

    let cannon_flashblocks_stream = nodes[0]
        .ext_context
        .clone()
        .unwrap()
        .flashblocks_handle
        .flashblock_stream();

    let (sender, mut rx) = tokio::sync::mpsc::channel(1);
    let timestamp = crate::setup::current_timestamp();
    let eip1559_params =
        encode_eip1559_params(nodes[0].node.inner.chain_spec().as_ref(), timestamp)?;

    // Note: Using a fixed timestamp for deterministic block hash in this test
    let attributes = build_payload_attributes(
        timestamp,
        eip1559_params,
        Some(vec![TX_SET_L1_BLOCK.clone()]),
    );

    let mine_block = crate::actions::AssertMineBlock::new(
        0,
        None,
        attributes,
        authorization_generator,
        std::time::Duration::from_millis(2000),
        true,
        sender,
    )
    .await;

    let blocks_found = Arc::new(AtomicUsize::new(0));
    let blocks_counter = blocks_found.clone();
    let eth_block_by_hash =
        crate::actions::GetBlockByHash::new(vec![0, 1], cannon_flashblocks_stream).on_blocks(
            move |blocks| {
                crate::actions::assert::all_some(&blocks, "pending block by hash")?;
                blocks_counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        );

    let mut action = crate::actions::EthApiAction::new(mine_block, eth_block_by_hash);
    let fut = async {
        let _ = action.execute(&mut env).await;
    };

    futures::future::select(Box::pin(fut), Box::pin(rx.recv())).await;

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
        setup(3, optimism_payload_attributes, true).await?;

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
#[ignore = "TODO: flaky - not sure what's causing this to fail"]
async fn test_selective_propagation_policy() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    // We disconnect Node 0 from Node 2 to prevent multi-hop forwarding in Part 1
    let (_, mut nodes, _tasks, _, _) = setup_with_tx_peers(
        3,
        optimism_payload_attributes,
        true,
        false,
        true,
    )
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
        setup_with_tx_peers(3, optimism_payload_attributes, true, true, true)
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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "flaky test"]
async fn test_continuous_block_production_with_validation() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    const NUM_BLOCKS: u64 = 10;
    const BLOCK_INTERVAL_MS: u64 = 2000;
    const TXS_PER_FLASHBLOCK: u64 = 20;

    let (_, mut nodes, _tasks, mut flashblocks_env, tx_spammer) =
        setup(3, optimism_payload_attributes, true).await?;

    // Setup: 1 basic verifier node for flashblock validation
    let (_, mut basic_validators, _tasks, _basic_env, _) =
        setup_with_tx_peers(
            1,
            optimism_payload_attributes,
            false,
            false,
            true,
        )
        .await?;

    let basic_validator = &mut basic_validators[0];

    let [builder_node, follower_0, follower_1] = &mut nodes[..] else {
        unreachable!("Expected exactly 2 nodes")
    };

    let builder_context = builder_node.ext_context.clone();

    let basic_beacon_handle =
        Arc::new(basic_validator.node.inner.consensus_engine_handle().clone());

    let follower_context_0 = follower_0.ext_context.clone();
    let _follower_context_1 = follower_1.ext_context.clone();

    // Create shared state for cross-action communication
    let state = BlockProductionState::new();

    // Create authorization generator
    let genesis_hash = builder_node.node.block_hash(0);
    let authorization_generator = crate::setup::create_authorization_generator(
        genesis_hash,
        builder_context
            .unwrap()
            .flashblocks_handle
            .builder_sk()
            .unwrap()
            .verifying_key(),
    );

    // Spawn spammer with shared state to track tx hashes
    let rpc_url = builder_node.node.rpc_url();
    tx_spammer.spawn_with_state(TXS_PER_FLASHBLOCK, rpc_url, state.clone());

    info!(
        target: "test",
        "Starting continuous block production test for {} blocks using ActionSequence",
        NUM_BLOCKS
    );

    // Track statistics via hooks
    let blocks_produced = Arc::new(AtomicU64::new(0));
    let validated_hashes = Arc::new(AtomicUsize::new(0));
    let receipts_fetched = Arc::new(AtomicUsize::new(0));

    // Create timestamp state that advances with each iteration
    let timestamp = Arc::new(AtomicU64::new(crate::setup::current_timestamp()));
    let chain_spec = builder_node.node.inner.chain_spec().clone();

    // Clone handles for the attribute builder closure
    let timestamp_for_attrs = timestamp.clone();
    let chain_spec_for_attrs = chain_spec.clone();

    let validated_hashes_counter = validated_hashes.clone();
    let receipts_counter = receipts_fetched.clone();

    // Build the composable action sequence for ONE block cycle
    let block_cycle = ActionSequence::new()
        // 1. Reset state at start of each block
        .then(ResetState::new(state.clone()))
        // 2. Mine block + validate flashblocks in parallel
        .with(
            DynamicMineBlock::new(
                0, // builder node
                authorization_generator.clone(),
                state.clone(),
                move || {
                    let ts = timestamp_for_attrs.load(Ordering::SeqCst);
                    let eip1559 =
                        crate::setup::encode_eip1559_params(chain_spec_for_attrs.as_ref(), ts)
                            .unwrap();
                    crate::setup::build_payload_attributes(
                        ts,
                        eip1559,
                        Some(vec![crate::setup::TX_SET_L1_BLOCK.clone()]),
                    )
                },
            )
            .with_interval(Duration::from_millis(BLOCK_INTERVAL_MS)),
            DynamicValidateFlashblocks::new(
                follower_context_0.unwrap().flashblocks_handle.clone(),
                basic_beacon_handle,
                chain_spec.clone(),
                state.clone(),
            ),
        )
        // 3. Query validated blocks and receipts in parallel (AFTER mining/validation)
        .with(
            QueryValidatedBlocks::new(vec![0, 1], state.clone()).on_block(move |_| {
                validated_hashes_counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
            QueryTxReceipts::new(vec![0, 1], state.clone()).on_receipt(move |_| {
                receipts_counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
        )
        // 6. Log completion and track stats
        // Note: Skipping Canonicalize on follower nodes for now
        // because Isthmus V4 payloads don't work with RPC new_payload_v3
        .then({
            let counter = blocks_produced.clone();
            LogBlockComplete::new(state.clone()).on_complete(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        })
        // 8. Small delay between blocks
        .then(Sleep::millis(100));

    // Repeat the block cycle N times, advancing timestamp each iteration
    let timestamp_for_repeat = timestamp.clone();
    let mut action = block_cycle.repeat(NUM_BLOCKS).on_each(move |i| {
        // Advance timestamp for next block (after first iteration)
        if i > 0 {
            timestamp_for_repeat.fetch_add(2, Ordering::SeqCst);
        }
        info!(
            target: "test",
            iteration = i + 1,
            total = NUM_BLOCKS,
            "Starting block cycle"
        );

        Ok(())
    });

    // Execute the entire repeated sequence as a single action
    let fut = async { action.execute(&mut flashblocks_env).await };

    futures::future::select(
        Box::pin(fut),
        Box::pin(if blocks_produced.load(Ordering::SeqCst) == NUM_BLOCKS {
            Either::Left(futures::future::ready(()))
        } else {
            Either::Right(futures::future::pending::<()>())
        }),
    )
    .await;

    let final_blocks = blocks_produced.load(Ordering::SeqCst);
    let final_hashes = validated_hashes.load(Ordering::SeqCst);
    let final_receipts = receipts_fetched.load(Ordering::SeqCst);

    info!(
        target: "test",
        blocks_produced = final_blocks,
        validated_hashes = final_hashes,
        receipts_fetched = final_receipts,
        "Test completed successfully"
    );

    assert_eq!(
        final_blocks, NUM_BLOCKS,
        "Should have produced {} blocks",
        NUM_BLOCKS
    );

    assert!(
        final_hashes > 0,
        "Should have validated at least some block hashes"
    );

    Ok(())
}
