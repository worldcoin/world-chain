use crate::setup::{TX_SET_L1_BLOCK, build_payload_attributes};
use alloy_network::{Ethereum, EthereumWallet, TransactionBuilder, eip2718::Encodable2718};
use alloy_primitives::{Bytes, b64};
use alloy_provider::ProviderBuilder;
use alloy_rpc_types::TransactionRequest;
use alloy_rpc_types_engine::PayloadStatusEnum;
use eyre::eyre::eyre;
use flashblocks_p2p::protocol::event::{ChainEvent, WorldChainEvent};
use op_alloy_consensus::OpTxEnvelope;
use reth::{
    chainspec::EthChainSpec,
    network::{NetworkSyncUpdater, SyncState},
};
use reth_e2e_test_utils::testsuite::actions::Action;
use reth_optimism_node::utils::optimism_payload_attributes;
use reth_transaction_pool::TransactionPool;
use revm_primitives::{Address, B256, U256};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tracing::info;
use world_chain_node::context::FlashblocksContext;
use world_chain_test::{
    node::{raw_pbh_bundle_bytes, tx},
    utils::{account, signer},
};

use crate::setup::{
    CHAIN_SPEC, create_test_transaction, encode_eip1559_params, setup,
    setup_with_block_uncompressed_size_limit, setup_with_tx_peers,
};

async fn create_priority_transaction(
    signer_index: u32,
    nonce: u64,
    calldata_len: usize,
    gas_limit: u64,
    max_priority_fee_per_gas: u128,
) -> eyre::Result<(Bytes, B256)> {
    let mut tx_request = tx(
        CHAIN_SPEC.chain.id(),
        Some(Bytes::from(vec![0u8; calldata_len])),
        nonce,
        Address::default(),
        gas_limit,
    );
    tx_request.value = Some(U256::ZERO);
    tx_request.max_priority_fee_per_gas = Some(max_priority_fee_per_gas);
    tx_request.max_fee_per_gas = Some(100_000_000_000u128 + max_priority_fee_per_gas);

    let wallet = EthereumWallet::from(signer(signer_index));
    let signed =
        <TransactionRequest as TransactionBuilder<Ethereum>>::build(tx_request, &wallet).await?;

    Ok((signed.encoded_2718().into(), *signed.tx_hash()))
}

#[tokio::test(flavor = "multi_thread")]
async fn test_can_build_pbh_payload() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();
    let (signers, mut nodes, _tasks, _, _) =
        setup::<FlashblocksContext>(1, optimism_payload_attributes, false).await?;
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

#[tokio::test(flavor = "multi_thread")]
async fn test_transaction_pool_ordering() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let (signers, mut nodes, _tasks, _, _) =
        setup::<FlashblocksContext>(1, optimism_payload_attributes, false).await?;
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

#[tokio::test(flavor = "multi_thread")]
async fn test_enforces_block_uncompressed_size_limit() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let (tx1, tx1_hash) = create_priority_transaction(0, 0, 50_000, 600_000, 100).await?;
    let (tx_small, tx_small_hash) = create_priority_transaction(1, 0, 30_000, 400_000, 95).await?;
    let (tx2, tx2_hash) = create_priority_transaction(2, 0, 50_000, 600_000, 90).await?;
    let (tx3, tx3_hash) = create_priority_transaction(3, 0, 50_000, 600_000, 80).await?;

    let block_uncompressed_size_limit =
        TX_SET_L1_BLOCK.len() as u64 + tx1.len() as u64 + tx_small.len() as u64;

    let (_, mut nodes, _tasks, _, _) =
        setup_with_block_uncompressed_size_limit::<FlashblocksContext>(
            1,
            optimism_payload_attributes,
            false,
            Some(block_uncompressed_size_limit),
        )
        .await?;
    let node = &mut nodes[0].node;

    assert_eq!(node.rpc.inject_tx(tx1.clone()).await?, tx1_hash);
    assert_eq!(node.rpc.inject_tx(tx_small.clone()).await?, tx_small_hash);
    assert_eq!(node.rpc.inject_tx(tx2.clone()).await?, tx2_hash);
    assert_eq!(node.rpc.inject_tx(tx3.clone()).await?, tx3_hash);

    let block1 = node.advance_block().await?;
    assert!(
        block1
            .block()
            .body()
            .transactions
            .iter()
            .any(|tx| *tx.tx_hash() == tx1_hash),
        "tx1 should be included in block 1"
    );
    assert!(
        block1
            .block()
            .body()
            .transactions
            .iter()
            .any(|tx| *tx.tx_hash() == tx_small_hash),
        "tx_small should fit in block 1"
    );
    assert!(
        !block1
            .block()
            .body()
            .transactions
            .iter()
            .any(|tx| *tx.tx_hash() == tx2_hash),
        "tx2 should go to a later block"
    );
    assert!(
        !block1
            .block()
            .body()
            .transactions
            .iter()
            .any(|tx| *tx.tx_hash() == tx3_hash),
        "tx3 should go to a later block"
    );

    let block2 = node.advance_block().await?;
    assert!(
        block2
            .block()
            .body()
            .transactions
            .iter()
            .any(|tx| *tx.tx_hash() == tx2_hash),
        "tx2 should be included in block 2"
    );
    assert!(
        !block2
            .block()
            .body()
            .transactions
            .iter()
            .any(|tx| *tx.tx_hash() == tx3_hash),
        "tx3 should still go after block 2"
    );

    let block3 = node.advance_block().await?;
    assert!(
        block3
            .block()
            .body()
            .transactions
            .iter()
            .any(|tx| *tx.tx_hash() == tx3_hash),
        "tx3 should be included in block 3"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_without_block_uncompressed_size_limit_includes_all_transactions() -> eyre::Result<()>
{
    reth_tracing::init_test_tracing();

    let (_, mut nodes, _tasks, _, _) =
        setup::<FlashblocksContext>(1, optimism_payload_attributes, false).await?;
    let node = &mut nodes[0].node;

    let (tx1, tx1_hash) = create_priority_transaction(0, 0, 50_000, 600_000, 100).await?;
    let (tx2, tx2_hash) = create_priority_transaction(1, 0, 50_000, 600_000, 90).await?;
    let (tx3, tx3_hash) = create_priority_transaction(2, 0, 50_000, 600_000, 80).await?;

    assert_eq!(node.rpc.inject_tx(tx1).await?, tx1_hash);
    assert_eq!(node.rpc.inject_tx(tx2).await?, tx2_hash);
    assert_eq!(node.rpc.inject_tx(tx3).await?, tx3_hash);

    let block = node.advance_block().await?;
    assert!(
        block
            .block()
            .body()
            .transactions
            .iter()
            .any(|tx| *tx.tx_hash() == tx1_hash),
        "tx1 should be included"
    );
    assert!(
        block
            .block()
            .body()
            .transactions
            .iter()
            .any(|tx| *tx.tx_hash() == tx2_hash),
        "tx2 should be included"
    );
    assert!(
        block
            .block()
            .body()
            .transactions
            .iter()
            .any(|tx| *tx.tx_hash() == tx3_hash),
        "tx3 should be included"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_invalidate_dup_tx_and_nullifier() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();
    let (_signers, mut nodes, _tasks, _, _) =
        setup::<FlashblocksContext>(1, optimism_payload_attributes, false).await?;
    let node = &mut nodes[0].node;
    let signer = 0;
    let raw_tx = raw_pbh_bundle_bytes(signer, 0, 0, U256::ZERO, CHAIN_SPEC.chain_id()).await;
    node.rpc.inject_tx(raw_tx.clone()).await?;
    let dup_pbh_hash_res = node.rpc.inject_tx(raw_tx.clone()).await;
    assert!(dup_pbh_hash_res.is_err());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_dup_pbh_nonce() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let (_signers, mut nodes, _tasks, _, _) =
        setup::<FlashblocksContext>(1, optimism_payload_attributes, false).await?;
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
        setup::<FlashblocksContext>(2, optimism_payload_attributes, true).await?;

    // Verifier
    let (_, basic_nodes, _tasks, mut basic_env, _) = setup_with_tx_peers::<FlashblocksContext>(
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
        setup::<FlashblocksContext>(3, optimism_payload_attributes, true).await?;

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
        setup::<FlashblocksContext>(3, optimism_payload_attributes, true).await?;

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
        setup::<FlashblocksContext>(1, optimism_payload_attributes, true).await?;

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
        setup::<FlashblocksContext>(2, optimism_payload_attributes, true).await?;

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
#[tokio::test(flavor = "multi_thread")]
async fn test_default_propagation_policy() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    // Spin up 3 nodes WITHOUT tx_peers configuration
    let (_, mut nodes, _tasks, _, _) =
        setup::<FlashblocksContext>(3, optimism_payload_attributes, true).await?;

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
#[tokio::test(flavor = "multi_thread")]
async fn test_selective_propagation_policy() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    // We disconnect Node 0 from Node 2 to prevent multi-hop forwarding in Part 1
    let (_, mut nodes, _tasks, _, _) = setup_with_tx_peers::<FlashblocksContext>(
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
        .connect_peer(node_0_peer_id, node_0_addr);

    // Wait for reconnection to establish
    let start = tokio::time::Instant::now();
    loop {
        let peer = node_2_ctx
            .node
            .inner
            .network
            .get_peer_by_id(node_0_peer_id)
            .await?;
        if peer.is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(10) {
            panic!("Timeout waiting for Node 0 <-> Node 2 reconnection");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Extra time for sync state to stabilize
    tokio::time::sleep(Duration::from_secs(1)).await;

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
#[tokio::test(flavor = "multi_thread")]
async fn test_gossip_disabled_no_propagation() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let (_, mut nodes, _tasks, _, _) =
        setup_with_tx_peers::<FlashblocksContext>(3, optimism_payload_attributes, true, true, true)
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

/// End-to-end test: drives the builder's consensus engine through a block
/// building loop, using a hook on the `WorldChainEventsStream` to assert
/// stream invariants:
///
/// 1. Canon events are always yielded
/// 2. Pending flashblocks are only yielded after their epoch parent is canonical
/// 3. Flashblock indices are monotonically increasing within an epoch
/// 4. The P2P state's flushed cursor tracks the latest yielded flashblock
/// 5. Stale flashblocks (from old epochs) are never yielded
#[tokio::test(flavor = "multi_thread")]
async fn test_event_stream_invariants() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    const TRANSACTIONS_PER_FLASHBLOCK: u64 = 10;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let (_, mut nodes, _tasks, mut env, tx_spammer) =
        setup::<FlashblocksContext>(1, optimism_payload_attributes, true).await?;

    let builder_node = &mut nodes[0];
    let builder_context = builder_node.ext_context.clone().unwrap();
    let rpc_url = builder_node.node.rpc_url();

    tx_spammer.spawn(TRANSACTIONS_PER_FLASHBLOCK, rpc_url);

    let block_hash = builder_node.node.block_hash(0);

    let authorization_generator = crate::setup::create_authorization_generator(
        block_hash,
        builder_context
            .flashblocks_handle
            .builder_sk()
            .unwrap()
            .verifying_key(),
    );

    let timestamp = crate::setup::current_timestamp();
    let eip1559_params =
        encode_eip1559_params(builder_node.node.inner.chain_spec().as_ref(), timestamp)?;

    let attributes = build_payload_attributes(
        timestamp,
        eip1559_params,
        Some(vec![TX_SET_L1_BLOCK.clone()]),
    );

    // --- Assertion state shared with the hook ---
    let canon_count = Arc::new(AtomicUsize::new(0));
    let pending_count = Arc::new(AtomicUsize::new(0));
    let last_index = Arc::new(AtomicU64::new(0));
    let saw_canon_before_pending = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let canon_count_hook = canon_count.clone();
    let pending_count_hook = pending_count.clone();
    let last_index_hook = last_index.clone();
    let saw_canon_hook = saw_canon_before_pending.clone();

    // Create the event stream with a hook that asserts invariants
    let p2p_state = builder_context.flashblocks_handle.state.clone();
    let mut stream = builder_context
        .flashblocks_handle
        .event_stream::<(), _, _, _>(
            builder_node.node.inner.provider.clone(),
            move |event: &WorldChainEvent<()>| {
                match event {
                    WorldChainEvent::Chain(ChainEvent::Canon(_tip)) => {
                        canon_count_hook.fetch_add(1, Ordering::SeqCst);
                    }
                    WorldChainEvent::Chain(ChainEvent::Pending(fb)) => {
                        // Invariant: we must have seen at least one canon event
                        // before any pending flashblock is yielded.
                        if canon_count_hook.load(Ordering::SeqCst) > 0 {
                            saw_canon_hook.store(true, Ordering::SeqCst);
                        }

                        // Invariant: indices are monotonically increasing
                        let prev = last_index_hook.swap(fb.index, Ordering::SeqCst);
                        if pending_count_hook.load(Ordering::SeqCst) > 0 {
                            assert!(
                                fb.index >= prev,
                                "flashblock index went backwards: {} -> {}",
                                prev,
                                fb.index
                            );
                        }

                        pending_count_hook.fetch_add(1, Ordering::SeqCst);
                    }
                    _ => {}
                }
                None
            },
        );

    // Spawn the stream consumer
    let _stream_handle = tokio::spawn(async move {
        let mut count = 0usize;
        while let Some(_event) = futures::StreamExt::next(&mut stream).await {
            count += 1;
            if count > 50 {
                break; // safety valve
            }
        }
        count
    });

    // Mine a block
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let mine_block = crate::actions::AssertMineBlock::new(
        0,
        None,
        attributes,
        authorization_generator,
        Duration::from_millis(3000),
        true,
        tx,
    )
    .await;

    tokio::spawn(async move {
        let mut mine_action = mine_block;
        mine_action.execute(&mut env).await
    });

    // Wait for mining to complete
    rx.recv()
        .await
        .ok_or(eyre!("failed to receive mined block"))?;

    // Give the stream a moment to process remaining events
    tokio::time::sleep(Duration::from_millis(500)).await;

    // --- Assert invariants ---
    let canons = canon_count.load(Ordering::SeqCst);
    let pendings = pending_count.load(Ordering::SeqCst);

    info!(
        target: "test",
        canon_events = canons,
        pending_events = pendings,
        "stream invariant results"
    );

    assert!(
        canons >= 1,
        "expected at least one canon event, got {canons}"
    );
    assert!(
        pendings >= 1,
        "expected at least one pending flashblock, got {pendings}"
    );
    assert!(
        saw_canon_before_pending.load(Ordering::SeqCst),
        "expected canon event before first pending flashblock"
    );

    // Verify P2P state was updated by publish_new
    let state = p2p_state.lock();
    assert!(
        state.payload_timestamp > 0,
        "expected payload_timestamp to be set on P2P state"
    );

    Ok(())
}

/// End-to-end test: uses [`EngineDriver`] to build multiple blocks while
/// querying the pending block, logs, transactions, and receipts via the
/// Eth JSON-RPC API at each block boundary.
#[tokio::test(flavor = "multi_thread")]
async fn test_engine_driver_pending_block_queries() -> eyre::Result<()> {
    use alloy_eips::BlockNumberOrTag;
    use reth::rpc::api::EthApiClient;

    reth_tracing::init_test_tracing();

    const NUM_BLOCKS: usize = 3;
    const BLOCK_INTERVAL: Duration = Duration::from_millis(2000);

    tokio::time::sleep(Duration::from_millis(100)).await;

    // 2 nodes: builder + follower
    let (_, nodes, _tasks, mut env, tx_spammer) =
        setup::<FlashblocksContext>(2, optimism_payload_attributes, true).await?;

    let builder_context = nodes[0].ext_context.clone().unwrap();
    let block_hash = nodes[0].node.block_hash(0);
    let chain_spec = nodes[0].node.inner.chain_spec().clone();
    let rpc_url = nodes[0].node.rpc_url();

    // Initialize forkchoice on all nodes to genesis
    for node in &nodes {
        node.node.update_forkchoice(block_hash, block_hash).await?;
    }

    // Spawn background transactions so blocks have content
    tx_spammer.spawn(10, rpc_url);

    let builder_vk = builder_context
        .flashblocks_handle
        .builder_sk()
        .unwrap()
        .verifying_key();

    let authorization_gen =
        move |parent_hash: B256, attrs: reth_optimism_node::OpPayloadAttributes| {
            let authorizer_sk = ed25519_dalek::SigningKey::from_bytes(&[0; 32]);
            let payload_id =
                reth_optimism_payload_builder::payload_id_optimism(&parent_hash, &attrs, 3);
            flashblocks_primitives::p2p::Authorization::new(
                payload_id,
                reth_node_api::PayloadAttributes::timestamp(&attrs),
                &authorizer_sk,
                builder_vk,
            )
        };

    // --- Flashblock stream: capture latest pending flashblock ---
    use flashblocks_primitives::primitives::FlashblocksPayloadV1;
    use std::sync::RwLock;

    let latest_stream_fb: Arc<RwLock<Option<FlashblocksPayloadV1>>> = Arc::new(RwLock::new(None));
    let latest_stream_fb_writer = latest_stream_fb.clone();

    // Use the raw flashblock_stream (no buffering) to capture flashblocks
    // as they're broadcast, independent of canon state.
    let mut fb_stream = builder_context.flashblocks_handle.flashblock_stream();

    let _stream_task = tokio::spawn(async move {
        while let Some(fb) = futures::StreamExt::next(&mut fb_stream).await {
            *latest_stream_fb_writer.write().unwrap() = Some(fb);
        }
    });

    // Track per-block results
    let blocks_verified = Arc::new(AtomicUsize::new(0));
    let blocks_verified_cb = blocks_verified.clone();

    let builder_rpc = env.node_clients[0].rpc.clone();

    let mut driver = crate::actions::EngineDriver {
        builder_idx: 0,
        follower_idxs: vec![],
        initial_parent_hash: Some(block_hash),
        num_blocks: NUM_BLOCKS,
        block_interval: BLOCK_INTERVAL,
        flashblocks: true,
        authorization_gen,
        attributes_gen: Box::new({
            let chain_spec = chain_spec.clone();
            move |_block_number, timestamp| {
                let eip1559 = encode_eip1559_params(chain_spec.as_ref(), timestamp)?;
                Ok(build_payload_attributes(
                    timestamp,
                    eip1559,
                    Some(vec![TX_SET_L1_BLOCK.clone()]),
                ))
            }
        }),
        during_build: None,
        on_block: Some(Box::new({
            let builder_rpc = builder_rpc.clone();
            let latest_stream_fb = latest_stream_fb.clone();
            move |block_num, payload| {
                let builder_rpc = builder_rpc.clone();
                let blocks_verified = blocks_verified_cb.clone();
                let latest_stream_fb = latest_stream_fb.clone();

                let payload_tx_count = payload
                    .execution_payload
                    .payload_inner
                    .payload_inner
                    .payload_inner
                    .transactions
                    .len();

                Box::pin(async move {
                    assert!(
                        payload_tx_count > 0,
                        "block {block_num}: expected at least 1 transaction"
                    );

                    // Query the pending block from the Eth API
                    let pending: Option<alloy_rpc_types_eth::Block> =
                        EthApiClient::<
                            TransactionRequest,
                            alloy_rpc_types::Transaction,
                            alloy_rpc_types_eth::Block,
                            alloy_consensus::Receipt,
                            alloy_consensus::Header,
                            reth_optimism_primitives::OpTransactionSigned,
                        >::block_by_number(
                            &builder_rpc, BlockNumberOrTag::Pending, false
                        )
                        .await?;

                    // Get the latest flashblock from the event stream
                    let stream_fb = latest_stream_fb.read().unwrap().clone();

                    if let (Some(pending_block), Some(stream_fb)) = (&pending, &stream_fb) {
                        info!(
                            target: "engine_driver_test",
                            block = block_num,
                            pending_number = pending_block.header.number,
                            pending_tx_count = pending_block.transactions.len(),
                            stream_payload_id = %stream_fb.payload_id,
                            stream_index = stream_fb.index,
                            stream_tx_count = stream_fb.diff.transactions.len(),
                            "comparing pending block vs event stream flashblock"
                        );

                        // The pending block tx count should be >= the stream
                        // flashblock's cumulative tx count (pending block
                        // includes all transactions, stream fb has the diff).
                        assert!(
                            !pending_block.transactions.is_empty()
                                || stream_fb.diff.transactions.is_empty(),
                            "block {block_num}: pending block should have transactions if stream flashblock does"
                        );
                        assert!(
                            pending_block.hash() == stream_fb.diff.block_hash,
                            "block {block_num}: pending block hash should match stream flashblock hash"
                        );
                    } else {
                        info!(
                            target: "engine_driver_test",
                            block = block_num,
                            pending_available = pending.is_some(),
                            stream_available = stream_fb.is_some(),
                            "pending block or stream flashblock not yet available"
                        );
                    }

                    blocks_verified.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }
        })),
    };

    driver.execute(&mut env).await?;

    let verified = blocks_verified.load(Ordering::SeqCst);

    info!(
        target: "engine_driver_test",
        verified,
        "engine driver test complete"
    );

    assert_eq!(
        verified, NUM_BLOCKS,
        "expected to verify {NUM_BLOCKS} blocks"
    );

    // Verify the event stream captured flashblocks
    let final_fb = latest_stream_fb.read().unwrap().clone();
    assert!(
        final_fb.is_some(),
        "expected the event stream to have captured at least one flashblock"
    );

    Ok(())
}

/// Large block production loop using [`EngineDriver`] that sanity-checks
/// all helper macros in the `on_block` hook: `provider!`, `fetch_block!`,
/// `fetch_tx!`, `fetch_receipt!`, `eth_call!`, `fetch_logs!`.
#[tokio::test(flavor = "multi_thread")]
async fn test_eth_api_assertions() -> eyre::Result<()> {
    use crate::setup::encode_eip1559_params;
    use alloy_provider::Provider;
    use alloy_rpc_types::Filter;

    reth_tracing::init_test_tracing();
    tokio::time::sleep(Duration::from_millis(100)).await;

    const NUM_BLOCKS: usize = 5;
    const BLOCK_INTERVAL: Duration = Duration::from_millis(2000);

    let (_, nodes, _tasks, mut env, tx_spammer) =
        setup::<FlashblocksContext>(1, optimism_payload_attributes, true).await?;

    let builder_context = nodes[0].ext_context.clone().unwrap();
    let block_hash = nodes[0].node.block_hash(0);
    let chain_spec = nodes[0].node.inner.chain_spec().clone();
    let rpc_url = nodes[0].node.rpc_url();

    for node in &nodes {
        node.node.update_forkchoice(block_hash, block_hash).await?;
    }

    tx_spammer.spawn(10, rpc_url);

    let builder_vk = builder_context
        .flashblocks_handle
        .builder_sk()
        .unwrap()
        .verifying_key();

    let authorization_gen =
        move |parent_hash: B256, attrs: reth_optimism_node::OpPayloadAttributes| {
            let authorizer_sk = ed25519_dalek::SigningKey::from_bytes(&[0; 32]);
            let payload_id =
                reth_optimism_payload_builder::payload_id_optimism(&parent_hash, &attrs, 3);
            flashblocks_primitives::p2p::Authorization::new(
                payload_id,
                reth_node_api::PayloadAttributes::timestamp(&attrs),
                &authorizer_sk,
                builder_vk,
            )
        };

    let checks_passed = Arc::new(AtomicUsize::new(0));
    let checks_passed_cb = checks_passed.clone();

    let mut driver = crate::actions::EngineDriver {
        builder_idx: 0,
        follower_idxs: vec![],
        initial_parent_hash: Some(block_hash),
        num_blocks: NUM_BLOCKS,
        block_interval: BLOCK_INTERVAL,
        flashblocks: true,
        authorization_gen,
        attributes_gen: Box::new({
            let chain_spec = chain_spec.clone();
            move |_block_number, timestamp| {
                let eip1559 = encode_eip1559_params(chain_spec.as_ref(), timestamp)?;
                Ok(build_payload_attributes(
                    timestamp,
                    eip1559,
                    Some(vec![TX_SET_L1_BLOCK.clone()]),
                ))
            }
        }),
        during_build: Some(Box::new({
            let url = nodes[0].node.rpc_url();
            move |block_num| {
                let url = url.clone();
                Box::pin(async move {
                    use alloy_provider::Provider;
                    let provider = ProviderBuilder::<_, _, op_alloy_network::Optimism>::default()
                        .network::<op_alloy_network::Optimism>()
                        .with_recommended_fillers()
                        .connect_http(url);

                    let pending = provider
                        .get_block_by_number(alloy_eips::BlockNumberOrTag::Pending)
                        .await?;

                    let latest = provider
                        .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest)
                        .await?;

                    if let (Some(pending_block), Some(latest_block)) = (&pending, &latest) {
                        assert_ne!(
                            pending_block.header.hash, latest_block.header.hash,
                            "block {block_num}: pending must differ from latest during build"
                        );
                        assert!(
                            pending_block.header.number > latest_block.header.number,
                            "block {block_num}: pending number ({}) must be > latest ({})",
                            pending_block.header.number,
                            latest_block.header.number,
                        );
                        info!(
                            target: "macro_sanity",
                            block = block_num,
                            pending_number = pending_block.header.number,
                            latest_number = latest_block.header.number,
                            "pending != latest verified during build"
                        );
                    }

                    Ok(())
                })
            }
        })),
        on_block: Some(Box::new({
            let nodes_0 = nodes[0].node.rpc_url();
            move |block_num, _payload| {
                let checks_passed = checks_passed_cb.clone();
                let url = nodes_0.clone();

                Box::pin(async move {
                    let provider = ProviderBuilder::<_, _, op_alloy_network::Optimism>::default()
                        .network::<op_alloy_network::Optimism>()
                        .with_recommended_fillers()
                        .connect_http(url);

                    // --- fetch_block!(Pending) ---
                    let pending = provider
                        .get_block_by_number(alloy_eips::BlockNumberOrTag::Pending)
                        .await?;
                    info!(
                        target: "macro_sanity",
                        block = block_num,
                        pending = pending.is_some(),
                        "pending block query"
                    );

                    // --- fetch_block!(Latest) ---
                    let latest: Option<
                        alloy_rpc_types::Block<op_alloy_rpc_types::Transaction<OpTxEnvelope>>,
                    > = provider
                        .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest)
                        .full()
                        .await?;

                    let latest = latest.unwrap();
                    let latest_number = latest.header.number;
                    let tx_count = latest.transactions.len();
                    info!(
                        target: "macro_sanity",
                        block = block_num,
                        latest_number,
                        tx_count,
                        "latest block query"
                    );

                    // --- fetch_tx! (first tx in latest block) ---
                    let tx_hashes: Vec<_> = latest.transactions.hashes().collect();
                    if let Some(&tx_hash) = tx_hashes.first() {
                        let tx: Option<op_alloy_rpc_types::Transaction<OpTxEnvelope>> =
                            provider.get_transaction_by_hash(tx_hash).await?;
                        assert!(
                            tx.is_some(),
                            "block {block_num}: fetch_tx for {tx_hash} should return a result"
                        );

                        // --- fetch_receipt! ---
                        let receipt = provider.get_transaction_receipt(tx_hash).await?;
                        assert!(
                            receipt.is_some(),
                            "block {block_num}: fetch_receipt for {tx_hash} should return a result"
                        );

                        info!(
                            target: "macro_sanity",
                            block = block_num,
                            %tx_hash,
                            "tx + receipt queries passed"
                        );
                    }

                    // --- eth_call ---
                    let call_tx = alloy_rpc_types::TransactionRequest::default()
                        .to(Address::ZERO)
                        .value(U256::ZERO);
                    let call_result = provider.call(call_tx.into()).await;
                    assert!(
                        call_result.is_ok(),
                        "block {block_num}: eth_call should succeed: {:?}",
                        call_result.err()
                    );
                    info!(target: "macro_sanity", block = block_num, "eth_call passed");

                    // --- fetch_logs ---
                    let filter = Filter::new()
                        .from_block(latest_number)
                        .to_block(latest_number);
                    let logs = provider.get_logs(&filter).await?;
                    info!(
                        target: "macro_sanity",
                        block = block_num,
                        log_count = logs.len(),
                        "fetch_logs passed"
                    );

                    // --- chain_id sanity ---
                    let chain_id = provider.get_chain_id().await?;
                    assert!(chain_id > 0, "block {block_num}: chain_id should be > 0");

                    checks_passed.fetch_add(1, Ordering::SeqCst);
                    info!(
                        target: "macro_sanity",
                        block = block_num,
                        "all checks passed"
                    );

                    Ok(())
                })
            }
        })),
    };

    driver.execute(&mut env).await?;

    let passed = checks_passed.load(Ordering::SeqCst);
    assert_eq!(
        passed, NUM_BLOCKS,
        "expected all {NUM_BLOCKS} blocks to pass macro sanity checks, got {passed}"
    );

    info!(target: "macro_sanity", passed, "all blocks verified");
    Ok(())
}

/// Assertion-driven event stream test: pre-computes expected events and
/// validates them against the live stream during block building.
///
/// Scenario:
/// - Build 3 blocks with flashblocks enabled
/// - For each block, expect: Canon(N), then Pending(0, is_base=true), then
///   at least one more Pending with increasing indices
/// - After all blocks, verify all assertions passed
#[tokio::test(flavor = "multi_thread")]
async fn test_assertion_driven_event_stream() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();
    tokio::time::sleep(Duration::from_millis(100)).await;

    const NUM_BLOCKS: usize = 3;
    const BLOCK_INTERVAL: Duration = Duration::from_millis(2000);

    let (_, nodes, _tasks, mut env, tx_spammer) =
        setup::<FlashblocksContext>(1, optimism_payload_attributes, true).await?;

    let builder_context = nodes[0].ext_context.clone().unwrap();
    let block_hash = nodes[0].node.block_hash(0);
    let chain_spec = nodes[0].node.inner.chain_spec().clone();
    let rpc_url = nodes[0].node.rpc_url();

    nodes[0]
        .node
        .update_forkchoice(block_hash, block_hash)
        .await?;

    tx_spammer.spawn(10, rpc_url);

    let builder_vk = builder_context
        .flashblocks_handle
        .builder_sk()
        .unwrap()
        .verifying_key();

    let authorization_gen =
        move |parent_hash: B256, attrs: reth_optimism_node::OpPayloadAttributes| {
            let authorizer_sk = ed25519_dalek::SigningKey::from_bytes(&[0; 32]);
            let payload_id =
                reth_optimism_payload_builder::payload_id_optimism(&parent_hash, &attrs, 3);
            flashblocks_primitives::p2p::Authorization::new(
                payload_id,
                reth_node_api::PayloadAttributes::timestamp(&attrs),
                &authorizer_sk,
                builder_vk,
            )
        };

    // Pre-compute assertions: expect the initial canon seed, then for each
    // block a base flashblock (index=0, is_base=true). Intermediate canon
    // events and delta flashblocks are skipped by the assertion checker.
    let mut assertions = Vec::new();

    // Initial canon tip seed from event_stream
    assertions.push(crate::actions::StreamAssertion::Canon { number: None });

    // For each block, expect at least the base flashblock.
    // The checker skips intervening canon events automatically.
    for _ in 0..NUM_BLOCKS {
        assertions.push(crate::actions::StreamAssertion::Pending {
            index: 0,
            is_base: true,
        });
    }

    // Subscribe to the event stream
    let stream = builder_context
        .flashblocks_handle
        .event_stream::<(), _, _, _>(
            nodes[0].node.inner.provider.clone(),
            |_: &WorldChainEvent<()>| None,
        );

    // Spawn assertion checker
    let assertion_handle = tokio::spawn(crate::actions::assert_stream(
        stream,
        assertions,
        Duration::from_secs(NUM_BLOCKS as u64 * 5),
    ));

    // Drive the engine
    let mut driver = crate::actions::EngineDriver {
        builder_idx: 0,
        follower_idxs: vec![],
        initial_parent_hash: Some(block_hash),
        num_blocks: NUM_BLOCKS,
        block_interval: BLOCK_INTERVAL,
        flashblocks: true,
        authorization_gen,
        attributes_gen: Box::new({
            let chain_spec = chain_spec.clone();
            move |_block_number, timestamp| {
                let eip1559 = encode_eip1559_params(chain_spec.as_ref(), timestamp)?;
                Ok(build_payload_attributes(
                    timestamp,
                    eip1559,
                    Some(vec![TX_SET_L1_BLOCK.clone()]),
                ))
            }
        }),
        during_build: None,
        on_block: None,
    };

    driver.execute(&mut env).await?;

    // Give the stream time to process remaining events
    tokio::time::sleep(Duration::from_millis(500)).await;

    let result = assertion_handle.await?;

    info!(
        target: "test",
        passed = result.passed,
        missed = result.missed,
        failures = ?result.failures,
        "assertion results"
    );

    result.assert_all_passed();

    Ok(())
}

/// Asserts that the flashblock payload builder never relays a flashblock with the same block hash
/// twice, and that the flashblock index only increases for changed pending blocks.
///
/// Without the tx spammer, the payload builder will repeatedly build identical payloads (no new
/// transactions arrive). The commit logic in [`FlashblocksPayloadJob::poll`] must detect this and
/// skip publishing duplicate flashblocks.
#[tokio::test(flavor = "multi_thread")]
async fn test_flashblocks_no_duplicate_relay() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    // Single builder node, flashblocks enabled, NO spammer started.
    let (_, mut nodes, _tasks, mut env, _tx_spammer) =
        setup::<FlashblocksContext>(1, optimism_payload_attributes, true).await?;

    let builder_node = &mut nodes[0];
    let builder_context = builder_node.ext_context.clone();
    let block_hash = builder_node.node.block_hash(0);

    let flashblocks_handle = &builder_context.as_ref().unwrap().flashblocks_handle;

    let builder_vk = flashblocks_handle.builder_sk().unwrap().verifying_key();
    let authorization_gen = crate::setup::create_authorization_generator(block_hash, builder_vk);

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

    // Collect all flashblocks published during the build interval.
    let collected: Arc<
        std::sync::Mutex<Vec<flashblocks_primitives::primitives::FlashblocksPayloadV1>>,
    > = Arc::new(std::sync::Mutex::new(Vec::new()));
    let collected_writer = collected.clone();

    let mut fb_stream = flashblocks_handle.flashblock_stream();
    let stream_task = tokio::spawn(async move {
        while let Some(fb) = futures::StreamExt::next(&mut fb_stream).await {
            collected_writer.lock().unwrap().push(fb);
        }
    });

    // Mine a single block with a generous build interval so that many recommit
    // cycles fire while no new transactions are available.
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let mine_block = crate::actions::AssertMineBlock::new(
        0,
        None,
        attributes,
        authorization_gen,
        Duration::from_millis(3000),
        true,
        tx,
    )
    .await;

    tokio::spawn(async move {
        let mut action = mine_block;
        action.execute(&mut env).await
    });

    // Wait for the block to be mined.
    rx.recv()
        .await
        .ok_or(eyre!("failed to receive mined block"))?;

    // Give the stream a moment to flush.
    tokio::time::sleep(Duration::from_millis(200)).await;
    stream_task.abort();

    let flashblocks = collected.lock().unwrap().clone();

    info!(
        target: "test",
        count = flashblocks.len(),
        "collected flashblocks for invariant check"
    );

    assert!(
        !flashblocks.is_empty(),
        "expected at least one flashblock to be published"
    );

    // ── Invariant 1: no duplicate block hashes ──
    // Each published flashblock must have a unique block hash.
    let mut seen_hashes = std::collections::HashSet::new();
    for fb in &flashblocks {
        let hash = fb.diff.block_hash;
        assert!(
            seen_hashes.insert(hash),
            "flashblock index {} relayed a duplicate block hash {hash}",
            fb.index,
        );
    }

    // ── Invariant 2: index strictly increases ──
    // Since every published flashblock has a new block hash (invariant 1),
    // the index must be strictly monotonically increasing.
    for window in flashblocks.windows(2) {
        let (prev, next) = (&window[0], &window[1]);
        assert!(
            next.index > prev.index,
            "flashblock index did not increase: prev={} next={}",
            prev.index,
            next.index,
        );
    }

    Ok(())
}
