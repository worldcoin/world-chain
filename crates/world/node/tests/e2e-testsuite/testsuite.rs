use alloy_network::{eip2718::Encodable2718, Ethereum, EthereumWallet, TransactionBuilder};
use alloy_primitives::b64;
use alloy_rpc_types::TransactionRequest;
use futures::StreamExt;
use parking_lot::Mutex;
use reth::chainspec::EthChainSpec;
use reth::primitives::RecoveredBlock;
use reth_e2e_test_utils::testsuite::actions::Action;
use reth_e2e_test_utils::transaction::TransactionTestContext;
use reth_node_api::{Block, PayloadAttributes};
use reth_optimism_node::{utils::optimism_payload_attributes, OpPayloadAttributes};
use reth_optimism_payload_builder::payload_id_optimism;
use reth_optimism_primitives::OpTransactionSigned;
use revm_primitives::fixed_bytes;
use revm_primitives::{Address, Bytes, B256, U256};
use rollup_boost::{ed25519_dalek::SigningKey, Authorization};
use std::sync::Arc;
use std::vec;
use tracing::info;
use world_chain_test::utils::account;

use world_chain_builder_node::context::BasicContext;
use world_chain_builder_node::context::FlashblocksContext;
use world_chain_builder_node::{Flashblock, Flashblocks};
use world_chain_test::node::{raw_pbh_bundle_bytes, tx};
use world_chain_test::utils::signer;

use crate::setup::{setup, CHAIN_SPEC};

#[tokio::test]
async fn test_can_build_pbh_payload() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();
    let (signers, mut nodes, _tasks, _) =
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

    let (signers, mut nodes, _tasks, _) =
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
    let (_signers, mut nodes, _tasks, _) =
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

    let (_signers, mut nodes, _tasks, _) =
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
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let (_, mut nodes, _tasks, mut flashblocks_env) =
        setup::<FlashblocksContext>(3, optimism_payload_attributes).await?;

    let (_, mut basic_nodes, _tasks, mut basic_env) =
        setup::<BasicContext>(1, optimism_payload_attributes).await?;

    let basic_worldchain_node = basic_nodes.first_mut().unwrap();

    let ext_context_1 = nodes[0].ext_context.clone();
    let ext_context_2 = nodes[1].ext_context.clone();

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

    let ext_context = node.ext_context.clone();
    let block_hash = node.node.block_hash(0);

    let authorization_generator = move |attrs: OpPayloadAttributes| {
        let authorizer_sk = SigningKey::from_bytes(&[0; 32]);

        let payload_id = payload_id_optimism(&block_hash, &attrs, 3);

        Authorization::new(
            payload_id,
            attrs.timestamp(),
            &authorizer_sk,
            ext_context.builder_sk.verifying_key(),
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

// #[tokio::test(flavor = "multi_thread")]
// async fn test_eth_api_receipt() -> eyre::Result<()> {
//     reth_tracing::init_test_tracing();
//     let (_, nodes, _tasks, mut env) =
//         setup::<FlashblocksContext>(3, optimism_payload_attributes).await?;

//     let ext_context = nodes[0].ext_context.clone();

//     let block_hash = nodes[0].node.block_hash(0);

//     let authorization_generator = move |attrs: OpPayloadAttributes| {
//         let authorizer_sk = SigningKey::from_bytes(&[0; 32]);

//         let payload_id = payload_id_optimism(&block_hash, &attrs, 3);

//         Authorization::new(
//             payload_id,
//             attrs.timestamp(),
//             &authorizer_sk,
//             ext_context.builder_sk.verifying_key(),
//         )
//     };

//     let timestamp = std::time::SystemTime::now()
//         .duration_since(std::time::UNIX_EPOCH)
//         .unwrap()
//         .as_secs();

//     let (sender, _) = tokio::sync::mpsc::channel(1);

//     // Compose a Mine Block action with an eth_getTransactionReceipt action
//     let attributes = OpPayloadAttributes {
//         payload_attributes: alloy_rpc_types_engine::PayloadAttributes {
//             timestamp,
//             prev_randao: B256::random(),
//             suggested_fee_recipient: Address::random(),
//             withdrawals: Some(vec![]),
//             parent_beacon_block_root: Some(B256::ZERO),
//         },
//         transactions: Some(vec![]),
//         no_tx_pool: Some(false),
//         eip_1559_params: Some(b64!("0000000800000008")),
//         gas_limit: Some(30_000_000),
//     };

//     let mock_tx =
//         TransactionTestContext::transfer_tx(nodes[0].node.inner.chain_spec().chain_id(), signer(0))
//             .await;

//     let raw_tx: Bytes = mock_tx.encoded_2718().into();

//     nodes[0].node.rpc.inject_tx(raw_tx.clone()).await?;

//     let mine_block = crate::actions::AssertMineBlock::new(
//         0,
//         vec![raw_tx],
//         Some(B256::ZERO),
//         attributes,
//         authorization_generator,
//         std::time::Duration::from_millis(2000),
//         true,
//         false,
//         sender,
//     )
//     .await;

//     let (tx, mut rx) = tokio::sync::mpsc::channel(1);

//     // 200ms backoff should be enough time to fetch the pending receipt
//     let transaction_receipt =
//         crate::actions::EthGetTransactionReceipt::new(*mock_tx.hash(), vec![0, 1, 2], 230, tx);

//     let mut action = crate::actions::EthApiAction::new(mine_block, transaction_receipt);
//     action.execute(&mut env).await?;

//     let _receipts = rx.recv().await.expect("should receive receipts");
//     info!("Receipts: {:?}", _receipts);
//     // TODO: Assertions once EthApi is fixed
//     Ok(())
// }

#[tokio::test(flavor = "multi_thread")]
async fn test_eth_api_call() -> eyre::Result<()> {
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let (_, nodes, _tasks, mut env) =
        setup::<FlashblocksContext>(3, optimism_payload_attributes).await?;

    let ext_context = nodes[0].ext_context.clone();

    let block_hash = nodes[0].node.block_hash(0);

    let authorization_generator = move |attrs: OpPayloadAttributes| {
        let authorizer_sk = SigningKey::from_bytes(&[0; 32]);

        let payload_id = payload_id_optimism(&block_hash, &attrs, 3);

        Authorization::new(
            payload_id,
            attrs.timestamp(),
            &authorizer_sk,
            ext_context.builder_sk.verifying_key(),
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
async fn test_eth_block_by_hash_pending() -> eyre::Result<()> {
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let (_, nodes, _tasks, mut env) =
        setup::<FlashblocksContext>(2, optimism_payload_attributes).await?;

    let ext_context = nodes[0].ext_context.clone();

    let block_hash = nodes[0].node.block_hash(0);

    let authorization_generator = move |attrs: OpPayloadAttributes| {
        let authorizer_sk = SigningKey::from_bytes(&[0; 32]);

        let payload_id = payload_id_optimism(&block_hash, &attrs, 3);

        Authorization::new(
            payload_id,
            attrs.timestamp(),
            &authorizer_sk,
            ext_context.builder_sk.verifying_key(),
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

    let eth_block_by_hash = crate::actions::EthGetBlockByHash::new(pending_hash, vec![0], 300, tx);
    let mut action = crate::actions::EthApiAction::new(mine_block, eth_block_by_hash);

    action.execute(&mut env).await?;

    // ensure the pre-confirmed block exists on the path of the pending tag
    let mut blocks = rx.recv().await.expect("should receive block");
    blocks.pop().expect("should have one block").unwrap();

    Ok(())
}

// TODO: Mock failover scenario test
// - Assert Mined block of both nodes is identical in a failover scenario for FCU's with the same parent attributes
