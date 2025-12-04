use alloy_network::{eip2718::Encodable2718, Ethereum, EthereumWallet, TransactionBuilder};
use alloy_primitives::b64;
use alloy_rpc_types::TransactionRequest;
use ed25519_dalek::SigningKey;
use flashblocks_primitives::p2p::Authorization;
use futures::StreamExt;
use op_alloy_consensus::encode_holocene_extra_data;
use parking_lot::Mutex;
use reth::{
    chainspec::EthChainSpec,
    network::{NetworkSyncUpdater, SyncState},
    primitives::RecoveredBlock,
};
use reth_e2e_test_utils::{testsuite::actions::Action, transaction::TransactionTestContext};
use reth_node_api::{Block, PayloadAttributes};
use reth_optimism_node::{utils::optimism_payload_attributes, OpPayloadAttributes};
use reth_optimism_payload_builder::payload_id_optimism;
use reth_optimism_primitives::OpTransactionSigned;
use reth_transaction_pool::TransactionPool;
use revm_primitives::{fixed_bytes, Address, Bytes, B256, U256};
use std::{sync::Arc, time::Duration, vec};
use tracing::info;
use world_chain_node::context::FlashblocksContext;
use world_chain_test::utils::account;

use flashblocks_primitives::flashblocks::{Flashblock, Flashblocks};
use world_chain_test::{
    node::{raw_pbh_bundle_bytes, tx},
    utils::signer,
};

use crate::setup::{setup, setup_with_tx_peers, CHAIN_SPEC};

#[tokio::test]
async fn test_can_build_pbh_payload() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();
    let (signers, mut nodes, _tasks, _) =
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

#[tokio::test]
async fn test_transaction_pool_ordering() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let (signers, mut nodes, _tasks, _) =
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

#[tokio::test]
async fn test_invalidate_dup_tx_and_nullifier() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();
    let (_signers, mut nodes, _tasks, _) =
        setup::<FlashblocksContext>(1, optimism_payload_attributes, false).await?;
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

    let (_signers, mut nodes, _tasks, _) =
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
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let (_, mut nodes, _tasks, mut flashblocks_env) =
        setup::<FlashblocksContext>(3, optimism_payload_attributes, true).await?;

    let (_, mut basic_nodes, _tasks, mut basic_env) =
        setup::<FlashblocksContext>(1, optimism_payload_attributes, false).await?;

    let basic_worldchain_node = basic_nodes.first_mut().unwrap();

    // Safe unwrap here because nodes 0 and 1 have flashblocks enabled
    let ext_context_1 = nodes[0].ext_context.clone().unwrap();
    let ext_context_2 = nodes[1].ext_context.clone().unwrap();

    let now = std::time::Instant::now();

    let flashblocks_0 = Arc::new(Mutex::new(Vec::new()));
    let flashblocks_1 = Arc::new(Mutex::new(Vec::new()));

    let flashblocks_0_clone = flashblocks_0.clone();
    let flashblocks_1_clone = flashblocks_1.clone();

    tokio::spawn(async move {
        let stream_0 = ext_context_1.flashblocks_handle.flashblock_stream();
        let stream_1 = ext_context_2.flashblocks_handle.flashblock_stream();

        futures::pin_mut!(stream_0);
        futures::pin_mut!(stream_1);

        while let (Some(flashblock_0), Some(flashblock_1)) =
            futures::future::join(stream_0.next(), stream_1.next()).await
        {
            let elapsed = now.elapsed();
            info!(
                "Received flashblocks after {:?}: 0: {:?}, 1: {:?}",
                elapsed.as_millis(),
                flashblock_0.payload_id,
                flashblock_1.payload_id
            );

            flashblocks_0.lock().push(flashblock_0);
            flashblocks_1.lock().push(flashblock_1);
        }
    });

    let node = &mut nodes[0];

    for i in 0..10 {
        let tx = TransactionTestContext::transfer_tx(
            node.node.inner.chain_spec().chain_id(),
            signer(i as u32),
        )
        .await;
        let envelope = TransactionTestContext::sign_tx(signer(i as u32), tx.into()).await;
        let tx: Bytes = envelope.encoded_2718().into();

        let _ = tokio::join!(
            node.node.rpc.inject_tx(tx.clone()),
            basic_worldchain_node.node.rpc.inject_tx(tx)
        );
    }

    // Safe unwrap because node 0 has flashblocks enabled
    let ext_context = node.ext_context.clone().unwrap();
    let block_hash = node.node.block_hash(0);

    let authorization_generator = move |attrs: OpPayloadAttributes| {
        let authorizer_sk = SigningKey::from_bytes(&[0; 32]);

        let payload_id = payload_id_optimism(&block_hash, &attrs, 3);

        Authorization::new(
            payload_id,
            attrs.timestamp(),
            &authorizer_sk,
            ext_context
                .flashblocks_handle
                .builder_sk()
                .unwrap()
                .verifying_key(),
        )
    };

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let attributes = OpPayloadAttributes {
        payload_attributes: alloy_rpc_types_engine::PayloadAttributes {
            timestamp,
            prev_randao: B256::random(),
            suggested_fee_recipient: Address::random(),
            withdrawals: Some(vec![]),
            parent_beacon_block_root: Some(B256::ZERO),
        },
        transactions: None,
        no_tx_pool: Some(false),
        eip_1559_params: Some(b64!("0000000800000008")),
        gas_limit: Some(30_000_000),
        min_base_fee: None,
    };

    let _tx = tx.clone();

    let mut flashblocks_action = crate::actions::AssertMineBlock::new(
        0,
        vec![],
        Some(B256::ZERO),
        attributes.clone(),
        authorization_generator.clone(),
        std::time::Duration::from_millis(2000),
        true,
        true,
        tx.clone(),
    )
    .await;

    let mut basic_action = crate::actions::AssertMineBlock::new(
        0,
        vec![],
        Some(B256::ZERO),
        attributes.clone(),
        authorization_generator,
        std::time::Duration::from_millis(2000),
        false,
        true,
        tx,
    )
    .await;

    flashblocks_action.execute(&mut flashblocks_env).await?;

    let flashblocks_envelope = rx.recv().await.expect("should receive payload");

    basic_action.execute(&mut basic_env).await?;

    let basic_envelope = rx.recv().await.expect("should receive payload");

    let flashblock_block = flashblocks_envelope
        .execution_payload
        .try_into_block::<OpTransactionSigned>()
        .expect("valid block")
        .try_into_recovered()
        .expect("valid recovered block");

    let basic_block = basic_envelope
        .execution_payload
        .try_into_block::<OpTransactionSigned>()
        .expect("valid block")
        .try_into_recovered()
        .expect("valid recovered block");

    let hash = flashblock_block.hash_slow();
    let basic_hash = basic_block.hash_slow();

    assert_eq!(hash, basic_hash, "Blocks from both nodes should match");

    let aggregated_flashblocks_0 = Flashblock::reduce(
        Flashblocks::new(
            flashblocks_0_clone
                .lock()
                .iter()
                .map(|fb| Flashblock {
                    flashblock: fb.clone(),
                })
                .collect(),
        )
        .unwrap(),
    );

    let aggregated_flashblocks_1 = Flashblock::reduce(
        Flashblocks::new(
            flashblocks_1_clone
                .lock()
                .iter()
                .map(|fb| Flashblock {
                    flashblock: fb.clone(),
                })
                .collect(),
        )
        .unwrap(),
    );

    let block_0: RecoveredBlock<alloy_consensus::Block<OpTransactionSigned>> =
        RecoveredBlock::try_from(aggregated_flashblocks_0.unwrap())
            .expect("failed to recover block from flashblock 0");

    let block_1: RecoveredBlock<alloy_consensus::Block<OpTransactionSigned>> =
        RecoveredBlock::try_from(aggregated_flashblocks_1.unwrap())
            .expect("failed to recover block from flashblock 1");

    assert_eq!(
        block_0.hash_slow(),
        hash,
        "Flashblock 0 did not match mined block"
    );
    assert_eq!(
        block_1.hash_slow(),
        hash,
        "Flashblock 1 did not match mined block"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_eth_api_receipt() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();
    let (_, nodes, _tasks, mut env) =
        setup::<FlashblocksContext>(3, optimism_payload_attributes, true).await?;

    // Safe unwrap because nodes have flashblocks enabled
    let ext_context = nodes[0].ext_context.clone().unwrap();

    let block_hash = nodes[0].node.block_hash(0);

    let authorization_generator = move |attrs: OpPayloadAttributes| {
        let authorizer_sk = SigningKey::from_bytes(&[0; 32]);

        let payload_id = payload_id_optimism(&block_hash, &attrs, 3);

        Authorization::new(
            payload_id,
            attrs.timestamp(),
            &authorizer_sk,
            ext_context
                .flashblocks_handle
                .builder_sk()
                .unwrap()
                .verifying_key(),
        )
    };

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let (sender, _) = tokio::sync::mpsc::channel(1);

    let eip1559 = encode_holocene_extra_data(
        Default::default(),
        nodes[0]
            .node
            .inner
            .chain_spec()
            .base_fee_params_at_timestamp(timestamp),
    )?;

    // Compose a Mine Block action with an eth_getTransactionReceipt action
    let attributes = OpPayloadAttributes {
        payload_attributes: alloy_rpc_types_engine::PayloadAttributes {
            timestamp,
            prev_randao: B256::random(),
            suggested_fee_recipient: Address::random(),
            withdrawals: Some(vec![]),
            parent_beacon_block_root: Some(B256::ZERO),
        },
        transactions: Some(vec![crate::setup::TX_SET_L1_BLOCK.clone()]),
        no_tx_pool: Some(false),
        eip_1559_params: Some(eip1559[1..=8].try_into()?),
        gas_limit: Some(30_000_000),
        min_base_fee: None,
    };

    let mock_tx =
        TransactionTestContext::transfer_tx(nodes[0].node.inner.chain_spec().chain_id(), signer(0))
            .await;

    let raw_tx: Bytes = mock_tx.encoded_2718().into();

    nodes[0].node.rpc.inject_tx(raw_tx.clone()).await?;

    let mine_block = crate::actions::AssertMineBlock::new(
        0,
        vec![],
        None,
        attributes,
        authorization_generator,
        std::time::Duration::from_millis(2000),
        true,
        false,
        sender,
    )
    .await;

    let (tx, mut rx) = tokio::sync::mpsc::channel(1);

    let transaction_receipt =
        crate::actions::EthGetTransactionReceipt::new(*mock_tx.hash(), vec![0, 1, 2], 240, tx);

    let mut action = crate::actions::EthApiAction::new(mine_block, transaction_receipt);
    action.execute(&mut env).await?;

    let receipts = rx.recv().await.expect("should receive receipts");
    info!("Receipts: {:?}", receipts);

    assert_eq!(
        receipts.len(),
        3,
        "Should receive receipts from all 3 nodes"
    );

    for (idx, receipt_opt) in receipts.iter().enumerate() {
        assert!(
            receipt_opt.is_some(),
            "Node {} should return a receipt",
            idx
        );
    }

    let receipts: Vec<_> = receipts.into_iter().map(|r| r.unwrap()).collect();

    for (idx, receipt) in receipts.iter().enumerate() {
        assert!(
            receipt.inner.inner.status(),
            "Transaction should succeed on node {}",
            idx
        );
    }

    for (idx, receipt) in receipts.iter().enumerate().skip(1) {
        assert_eq!(
            receipt, &receipts[0],
            "Node {} receipt doesn't match node 0",
            idx
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_eth_api_call() -> eyre::Result<()> {
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let (_, nodes, _tasks, mut env) =
        setup::<FlashblocksContext>(3, optimism_payload_attributes, true).await?;

    // Safe unwrap because nodes have flashblocks enabled
    let ext_context = nodes[0].ext_context.clone().unwrap();

    let block_hash = nodes[0].node.block_hash(0);

    let authorization_generator = move |attrs: OpPayloadAttributes| {
        let authorizer_sk = SigningKey::from_bytes(&[0; 32]);

        let payload_id = payload_id_optimism(&block_hash, &attrs, 3);

        Authorization::new(
            payload_id,
            attrs.timestamp(),
            &authorizer_sk,
            ext_context
                .flashblocks_handle
                .builder_sk()
                .unwrap()
                .verifying_key(),
        )
    };

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

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

    let signer = EthereumWallet::from(signer(0));
    let envelope =
        <TransactionRequest as TransactionBuilder<Ethereum>>::build(mock_tx.clone(), &signer)
            .await
            .unwrap();

    let raw_tx: Bytes = envelope.encoded_2718().into();

    let attributes = OpPayloadAttributes {
        payload_attributes: alloy_rpc_types_engine::PayloadAttributes {
            timestamp,
            prev_randao: B256::random(),
            suggested_fee_recipient: Address::random(),
            withdrawals: Some(vec![]),
            parent_beacon_block_root: Some(B256::ZERO),
        },
        transactions: Some(vec![raw_tx.clone()]),
        no_tx_pool: Some(false),
        eip_1559_params: Some(b64!("0000000800000008")),
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

    let (_, _nodes, _tasks, mut env) =
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

    let (_, nodes, _tasks, mut env) =
        setup::<FlashblocksContext>(2, optimism_payload_attributes, true).await?;

    // Safe unwrap because nodes have flashblocks enabled
    let ext_context = nodes[0].ext_context.clone().unwrap();

    let block_hash = nodes[0].node.block_hash(0);

    let builder_sk = ext_context.flashblocks_handle.builder_sk().unwrap().clone();

    let authorization_generator = move |attrs: OpPayloadAttributes| {
        let authorizer_sk = SigningKey::from_bytes(&[0; 32]);

        let payload_id = payload_id_optimism(&block_hash, &attrs, 3);

        Authorization::new(
            payload_id,
            attrs.timestamp(),
            &authorizer_sk,
            builder_sk.verifying_key(),
        )
    };

    let (sender, _) = tokio::sync::mpsc::channel(1);

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
        eip_1559_params: Some(b64!("0000000800000008")),
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
        fixed_bytes!("f8e1bed42c0ef37d2452900e0fcdd638b857136651c91dd2f6492ceb56b44923");

    let eth_block_by_hash = crate::actions::EthGetBlockByHash::new(pending_hash, vec![0], 230, tx);
    let mut action = crate::actions::EthApiAction::new(mine_block, eth_block_by_hash);

    action.execute(&mut env).await?;

    // ensure the pre-confirmed block exists on the path of the pending tag
    let mut blocks = rx.recv().await.expect("should receive block");
    blocks.pop().expect("should have one block").unwrap();

    Ok(())
}

// TODO: Mock failover scenario test
// - Assert Mined block of both nodes is identical in a failover scenario for FCU's with the same parent attributes
//

// Transaction Propagation Tests

/// Test default transaction propagation behavior without tx_peers configuration
///
/// Verifies that without tx_peers configuration, transactions propagate to ALL connected peers
/// using Reth's default TransactionPropagationKind::All policy.
#[tokio::test]
async fn test_default_propagation_policy() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    // Spin up 3 nodes WITHOUT tx_peers configuration
    let (_, mut nodes, _tasks, _) =
        setup::<FlashblocksContext>(3, optimism_payload_attributes, true).await?;

    let [node_0_ctx, node_1_ctx, node_2_ctx] = &mut nodes[..] else {
        unreachable!()
    };
    let node_0 = &mut node_0_ctx.node;
    let node_1 = &mut node_1_ctx.node;
    let node_2 = &mut node_2_ctx.node;

    // Set nodes to Idle state to enable transaction propagation
    node_0.inner.network.update_sync_state(SyncState::Idle);
    node_1.inner.network.update_sync_state(SyncState::Idle);
    node_2.inner.network.update_sync_state(SyncState::Idle);

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Create and inject transaction into Node 0
    let tx_request = tx(CHAIN_SPEC.chain.id(), None, 0, Address::default(), 210_000);
    let wallet = signer(0);
    let signer_wallet = EthereumWallet::from(wallet);
    let signed =
        <TransactionRequest as TransactionBuilder<Ethereum>>::build(tx_request, &signer_wallet)
            .await
            .unwrap();

    let raw_tx: Bytes = signed.encoded_2718().into();
    let tx_hash = *signed.tx_hash();

    let result = node_0.rpc.inject_tx(raw_tx.clone()).await;
    assert!(result.is_ok(), "Transaction should be accepted by Node 0");

    tokio::time::sleep(Duration::from_secs(5)).await;

    assert!(
        node_0.inner.pool().contains(&tx_hash),
        "Transaction should be in Node 0's pool"
    );

    assert!(
        node_1.inner.pool().contains(&tx_hash),
        "Transaction SHOULD propagate to Node 1 with default policy (TransactionPropagationKind::All)"
    );

    assert!(
        node_2.inner.pool().contains(&tx_hash),
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
    let (_, mut nodes, _tasks, _) = setup_with_tx_peers::<FlashblocksContext>(
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

    let node_0 = &mut node_0_ctx.node;
    let node_1 = &mut node_1_ctx.node;
    let node_2 = &mut node_2_ctx.node;

    let node_0_peer_id = node_0.network.record().id;
    let node_1_peer_id = node_1.network.record().id;
    let node_2_peer_id = node_2.network.record().id;

    // Set nodes to Idle state to enable transaction propagation
    use reth::network::{NetworkSyncUpdater, Peers, SyncState};
    node_0.inner.network.update_sync_state(SyncState::Idle);
    node_1.inner.network.update_sync_state(SyncState::Idle);
    node_2.inner.network.update_sync_state(SyncState::Idle);

    // Disconnect Node 0 from Node 2 to prevent multi-hop forwarding
    node_0.inner.network.disconnect_peer(node_2_peer_id);
    node_2.inner.network.disconnect_peer(node_0_peer_id);

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Create and inject transaction into Node 1 (which has tx_peers = [Node 0 only])
    let tx_request = tx(CHAIN_SPEC.chain.id(), None, 0, Address::default(), 210_000);
    let wallet = signer(0);
    let signer_wallet = EthereumWallet::from(wallet);
    let signed =
        <TransactionRequest as TransactionBuilder<Ethereum>>::build(tx_request, &signer_wallet)
            .await
            .unwrap();

    let raw_tx: Bytes = signed.encoded_2718().into();
    let tx_hash = *signed.tx_hash();

    let result = node_1.rpc.inject_tx(raw_tx.clone()).await;
    assert!(result.is_ok(), "Transaction should be accepted by Node 1");

    tokio::time::sleep(Duration::from_secs(3)).await;

    assert!(
        node_1.inner.pool().contains(&tx_hash),
        "Transaction should be in Node 1's pool"
    );

    assert!(
        node_0.inner.pool().contains(&tx_hash),
        "Transaction SHOULD propagate to Node 0 (whitelisted in Node 1's tx_peers)"
    );

    assert!(
        !node_2.inner.pool().contains(&tx_hash),
        "Transaction should NOT propagate to Node 2 (NOT in Node 1's tx_peers whitelist)"
    );

    // Part 2: Test Node 2 -> Node 0 and Node 1
    // Disconnect node 0 and node 1
    node_0.inner.network.disconnect_peer(node_1_peer_id);
    node_1.inner.network.disconnect_peer(node_0_peer_id);

    tokio::time::sleep(Duration::from_secs(3)).await;

    // Reconnect Node 0 and Node 2
    let node_0_addr = node_0.network.record().tcp_addr();
    node_2.inner.network.add_peer(node_0_peer_id, node_0_addr);

    tokio::time::sleep(Duration::from_secs(3)).await;

    // Create a new transaction and inject into Node 2
    // Node 2 has tx_peers = [Node 0, Node 1], so it should propagate to both
    let tx_request_2 = tx(CHAIN_SPEC.chain.id(), None, 0, Address::default(), 210_000);
    let wallet_2 = signer(1); // Different signer
    let signer_wallet_2 = EthereumWallet::from(wallet_2);
    let signed_2 =
        <TransactionRequest as TransactionBuilder<Ethereum>>::build(tx_request_2, &signer_wallet_2)
            .await
            .unwrap();

    let raw_tx_2: Bytes = signed_2.encoded_2718().into();
    let tx_hash_2 = *signed_2.tx_hash();

    // Inject transaction into Node 2
    let result = node_2.rpc.inject_tx(raw_tx_2.clone()).await;
    assert!(result.is_ok(), "Transaction should be accepted by Node 2");

    tokio::time::sleep(Duration::from_secs(2)).await;

    assert!(
        node_2.inner.pool().contains(&tx_hash_2),
        "Transaction should be in Node 2's pool"
    );

    assert!(
        node_0.inner.pool().contains(&tx_hash_2),
        "Transaction SHOULD propagate to Node 0 (whitelisted in Node 2's tx_peers)"
    );

    assert!(
        node_1.inner.pool().contains(&tx_hash_2),
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

    use crate::setup::setup_with_tx_peers;

    let (_, mut nodes, _tasks, _) =
        setup_with_tx_peers::<FlashblocksContext>(3, optimism_payload_attributes, true, true, true)
            .await?;

    let [node_0_ctx, node_1_ctx, node_2_ctx] = &mut nodes[..] else {
        unreachable!()
    };
    let node_0 = &mut node_0_ctx.node;
    let node_1 = &mut node_1_ctx.node;
    let node_2 = &mut node_2_ctx.node;

    // Set nodes to Idle state to enable transaction propagation
    node_0.inner.network.update_sync_state(SyncState::Idle);
    node_1.inner.network.update_sync_state(SyncState::Idle);
    node_2.inner.network.update_sync_state(SyncState::Idle);

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Inject transaction into Node 1 (which has tx_peers = [Node 0])
    // Even with tx_peers configured, gossip disabled should prevent propagation
    let tx_request_2 = tx(CHAIN_SPEC.chain.id(), None, 0, Address::default(), 210_000);
    let wallet_2 = signer(1);
    let signer_wallet_2 = EthereumWallet::from(wallet_2);
    let signed_2 =
        <TransactionRequest as TransactionBuilder<Ethereum>>::build(tx_request_2, &signer_wallet_2)
            .await
            .unwrap();

    let raw_tx_2: Bytes = signed_2.encoded_2718().into();
    let tx_hash_2 = *signed_2.tx_hash();

    let result = node_1.rpc.inject_tx(raw_tx_2.clone()).await;
    assert!(result.is_ok(), "Transaction should be accepted by Node 1");

    // Wait for potential propagation
    tokio::time::sleep(Duration::from_secs(3)).await;

    assert!(
        node_1.inner.pool().contains(&tx_hash_2),
        "Transaction should be in Node 1's pool"
    );

    assert!(
        !node_0.inner.pool().contains(&tx_hash_2),
        "Transaction should NOT propagate to Node 0 (gossip disabled takes precedence over tx_peers)"
    );

    assert!(
        !node_2.inner.pool().contains(&tx_hash_2),
        "Transaction should NOT propagate to Node 2 (gossip disabled)"
    );

    Ok(())
}
