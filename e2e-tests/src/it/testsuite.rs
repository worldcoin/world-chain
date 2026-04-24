use alloy_consensus::BlockHeader;
use alloy_network::{Ethereum, EthereumWallet, TransactionBuilder, eip2718::Encodable2718};
use alloy_primitives::{Bytes, b64};
use alloy_provider::ProviderBuilder;
use alloy_rpc_types::TransactionRequest;
use alloy_rpc_types_engine::PayloadStatusEnum;
use eyre::eyre::eyre;
use op_alloy_consensus::OpTxEnvelope;
use reth_chainspec::EthChainSpec;
use reth_e2e_test_utils::testsuite::actions::Action;
use reth_network::{NetworkSyncUpdater, SyncState};
use reth_optimism_node::utils::optimism_payload_attributes;
use reth_optimism_payload_builder::OpPayloadAttrs;
use reth_tasks::Runtime;
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
use world_chain_node::context::WorldChainDefaultContext;
use world_chain_p2p::protocol::event::{ChainEvent, WorldChainEvent};
use world_chain_test_utils::{
    e2e_harness::setup::{TX_SET_L1_BLOCK, build_payload_attributes},
    node::{raw_pbh_bundle_bytes, tx},
    utils::{account, signer},
};

use alloy_provider::{Provider, RootProvider};
use alloy_rpc_client::RpcClient;
use alloy_rpc_types_engine::PayloadId;
use ed25519_dalek::SigningKey;
use reth_network::{Peers, PeersInfo};
use reth_network_peers::PeerId;
use reth_tracing::tracing_subscriber::{self, util::SubscriberInitExt};
use std::{
    collections::HashMap,
    fmt,
    io::Write,
    time::{SystemTime, UNIX_EPOCH},
};
use tempfile::NamedTempFile;
use tokio::time::{Instant, sleep};
use tracing::Dispatch;
use world_chain_cli::FlashblocksArgs;
use world_chain_p2p::{
    monitor,
    protocol::{connection::ReceiveStatus, handler::PublishingStatus},
};
use world_chain_primitives::{
    flashblocks::FlashblockMetadata,
    p2p::{
        Authorization, Authorized, AuthorizedMsg, AuthorizedPayload, FlashblocksP2PMsg,
        StartPublish,
    },
    primitives::{ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1, FlashblocksPayloadV1},
};
use world_chain_test_utils::{
    e2e_harness::setup::{
        CHAIN_SPEC, create_test_transaction, encode_eip1559_params, setup,
        setup_with_block_uncompressed_size_limit, setup_with_tx_peers,
    },
    utils::{eip1559, raw_tx},
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
        setup::<WorldChainDefaultContext>(1, optimism_payload_attributes, false).await?;
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
        setup::<WorldChainDefaultContext>(1, optimism_payload_attributes, false).await?;
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
        setup_with_block_uncompressed_size_limit::<WorldChainDefaultContext>(
            1,
            optimism_payload_attributes,
            false,
            Some(block_uncompressed_size_limit),
            Arc::new(CHAIN_SPEC.clone()),
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
        setup::<WorldChainDefaultContext>(1, optimism_payload_attributes, false).await?;
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
        setup::<WorldChainDefaultContext>(1, optimism_payload_attributes, false).await?;
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
        setup::<WorldChainDefaultContext>(1, optimism_payload_attributes, false).await?;
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
        setup::<WorldChainDefaultContext>(2, optimism_payload_attributes, true).await?;

    // Verifier
    let (_, basic_nodes, _tasks, mut basic_env, _) =
        setup_with_tx_peers::<WorldChainDefaultContext>(
            1,
            optimism_payload_attributes,
            false,
            false,
            true,
            Arc::new(CHAIN_SPEC.clone()),
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

    let authorization_generator =
        world_chain_test_utils::e2e_harness::setup::create_authorization_generator(
            block_hash,
            builder_context
                .clone()
                .unwrap()
                .flashblocks_handle
                .builder_sk()
                .unwrap()
                .verifying_key(),
        );

    let timestamp = world_chain_test_utils::e2e_harness::setup::current_timestamp();

    let eip1559_params = world_chain_test_utils::e2e_harness::setup::encode_eip1559_params(
        builder_node.node.inner.chain_spec().as_ref(),
        timestamp,
    )?;

    let attributes = world_chain_test_utils::e2e_harness::setup::build_payload_attributes(
        timestamp,
        eip1559_params,
        Some(vec![
            world_chain_test_utils::e2e_harness::setup::TX_SET_L1_BLOCK.clone(),
        ]),
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel(1);

    let _tx = tx.clone();

    let mine_block = world_chain_test_utils::e2e_harness::actions::AssertMineBlock::new(
        0,
        None,
        attributes.into(),
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

    let validation_stream =
        world_chain_test_utils::e2e_harness::actions::FlashblocksValidatonStream {
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
        setup::<WorldChainDefaultContext>(3, optimism_payload_attributes, true).await?;

    let ext_context = nodes[0].ext_context.clone();
    let block_hash = nodes[0].node.block_hash(0);

    let authorization_generator =
        world_chain_test_utils::e2e_harness::setup::create_authorization_generator(
            block_hash,
            ext_context
                .unwrap()
                .flashblocks_handle
                .builder_sk()
                .unwrap()
                .verifying_key(),
        );

    let timestamp = world_chain_test_utils::e2e_harness::setup::current_timestamp();
    let (sender, mut block_rx) = tokio::sync::mpsc::channel(1);
    let eip1559_params = world_chain_test_utils::e2e_harness::setup::encode_eip1559_params(
        nodes[0].node.inner.chain_spec().as_ref(),
        timestamp,
    )?;

    // Compose a Mine Block action with an eth_getTransactionReceipt action
    let attributes = world_chain_test_utils::e2e_harness::setup::build_payload_attributes(
        timestamp,
        eip1559_params,
        Some(vec![
            world_chain_test_utils::e2e_harness::setup::TX_SET_L1_BLOCK.clone(),
        ]),
    );

    let cannon_flashblocks_stream = nodes[0]
        .ext_context
        .clone()
        .unwrap()
        .flashblocks_handle
        .flashblock_stream();

    let mine_block = world_chain_test_utils::e2e_harness::actions::AssertMineBlock::new(
        0,
        None,
        attributes,
        authorization_generator,
        std::time::Duration::from_millis(2000),
        true,
        sender,
    )
    .await;

    let transaction_receipt = world_chain_test_utils::e2e_harness::actions::GetReceipts::new(
        vec![0, 1, 2],
        cannon_flashblocks_stream,
    )
    .on_receipts(move |receipts| {
        for receipts in receipts {
            world_chain_test_utils::e2e_harness::actions::assert::all_some(
                &receipts,
                "transaction receipt",
            )?;
        }

        Ok(())
    });

    let mut action = world_chain_test_utils::e2e_harness::actions::EthApiAction::new(
        mine_block,
        transaction_receipt,
    );

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
        setup::<WorldChainDefaultContext>(3, optimism_payload_attributes, true).await?;

    let ext_context = nodes[0].ext_context.clone();
    let block_hash = nodes[0].node.block_hash(0);

    let authorization_generator =
        world_chain_test_utils::e2e_harness::setup::create_authorization_generator(
            block_hash,
            ext_context
                .unwrap()
                .flashblocks_handle
                .builder_sk()
                .unwrap()
                .verifying_key(),
        );

    let timestamp = world_chain_test_utils::e2e_harness::setup::current_timestamp();
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
    let raw_tx =
        world_chain_test_utils::e2e_harness::setup::sign_transaction(mock_tx.clone(), &wallet)
            .await;

    let attributes = world_chain_test_utils::e2e_harness::setup::build_payload_attributes(
        timestamp,
        b64!("0000000800000008"),
        Some(vec![raw_tx.clone()]),
    );

    let mine_block = world_chain_test_utils::e2e_harness::actions::AssertMineBlock::new(
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

    let eth_call =
        world_chain_test_utils::e2e_harness::actions::EthCall::new(mock_tx, vec![0, 1, 2], 200, tx);

    let mut action =
        world_chain_test_utils::e2e_harness::actions::EthApiAction::new(mine_block, eth_call);

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
        setup::<WorldChainDefaultContext>(1, optimism_payload_attributes, true).await?;

    let (tx, mut rx) = tokio::sync::mpsc::channel(1);

    let mut action =
        world_chain_test_utils::e2e_harness::actions::SupportedCapabilitiesCall::new(tx);

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
        setup::<WorldChainDefaultContext>(2, optimism_payload_attributes, true).await?;

    let ext_context = nodes[0].ext_context.clone();
    let block_hash = nodes[0].node.block_hash(0);

    let authorization_generator =
        world_chain_test_utils::e2e_harness::setup::create_authorization_generator(
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
    let timestamp = world_chain_test_utils::e2e_harness::setup::current_timestamp();
    let eip1559_params =
        encode_eip1559_params(nodes[0].node.inner.chain_spec().as_ref(), timestamp)?;

    // Note: Using a fixed timestamp for deterministic block hash in this test
    let attributes = build_payload_attributes(
        timestamp,
        eip1559_params,
        Some(vec![TX_SET_L1_BLOCK.clone()]),
    );

    let mine_block = world_chain_test_utils::e2e_harness::actions::AssertMineBlock::new(
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
    let eth_block_by_hash = world_chain_test_utils::e2e_harness::actions::GetBlockByHash::new(
        vec![0, 1],
        cannon_flashblocks_stream,
    )
    .on_blocks(move |blocks| {
        world_chain_test_utils::e2e_harness::actions::assert::all_some(
            &blocks,
            "pending block by hash",
        )?;
        blocks_counter.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });

    let mut action = world_chain_test_utils::e2e_harness::actions::EthApiAction::new(
        mine_block,
        eth_block_by_hash,
    );
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
        setup::<WorldChainDefaultContext>(3, optimism_payload_attributes, true).await?;

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
    let (raw_tx, tx_hash) =
        world_chain_test_utils::e2e_harness::setup::create_test_transaction(0, 0).await;

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
    let (_, mut nodes, _tasks, _, _) = setup_with_tx_peers::<WorldChainDefaultContext>(
        3,
        optimism_payload_attributes,
        true,
        false,
        true,
        Arc::new(CHAIN_SPEC.clone()),
    )
    .await?;

    let [node_0_ctx, node_1_ctx, node_2_ctx] = &mut nodes[..] else {
        unreachable!()
    };

    let node_0_peer_id = node_0_ctx.node.network.record().id;
    let node_1_peer_id = node_1_ctx.node.network.record().id;
    let node_2_peer_id = node_2_ctx.node.network.record().id;

    // Set nodes to Idle state to enable transaction propagation
    use reth_network_api::Peers;
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
    let (raw_tx, tx_hash) =
        world_chain_test_utils::e2e_harness::setup::create_test_transaction(0, 0).await;

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
    let (raw_tx_2, tx_hash_2) =
        world_chain_test_utils::e2e_harness::setup::create_test_transaction(1, 0).await;

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

    let (_, mut nodes, _tasks, _, _) = setup_with_tx_peers::<WorldChainDefaultContext>(
        3,
        optimism_payload_attributes,
        true,
        true,
        true,
        Arc::new(CHAIN_SPEC.clone()),
    )
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
        setup::<WorldChainDefaultContext>(1, optimism_payload_attributes, true).await?;

    let builder_node = &mut nodes[0];
    let builder_context = builder_node.ext_context.clone().unwrap();
    let rpc_url = builder_node.node.rpc_url();

    tx_spammer.spawn(TRANSACTIONS_PER_FLASHBLOCK, rpc_url);

    let block_hash = builder_node.node.block_hash(0);

    let authorization_generator =
        world_chain_test_utils::e2e_harness::setup::create_authorization_generator(
            block_hash,
            builder_context
                .flashblocks_handle
                .builder_sk()
                .unwrap()
                .verifying_key(),
        );

    let timestamp = world_chain_test_utils::e2e_harness::setup::current_timestamp();
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
    let mine_block = world_chain_test_utils::e2e_harness::actions::AssertMineBlock::new(
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
    use reth_rpc_api::EthApiClient;

    reth_tracing::init_test_tracing();

    const NUM_BLOCKS: usize = 3;
    const BLOCK_INTERVAL: Duration = Duration::from_millis(2000);

    tokio::time::sleep(Duration::from_millis(100)).await;

    // 2 nodes: builder + follower
    let (_, nodes, _tasks, mut env, tx_spammer) =
        setup::<WorldChainDefaultContext>(2, optimism_payload_attributes, true).await?;

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
        move |parent_hash: B256, attrs: reth_optimism_payload_builder::OpPayloadAttrs| {
            let authorizer_sk = ed25519_dalek::SigningKey::from_bytes(&[0; 32]);
            let payload_id =
                reth_optimism_payload_builder::payload_id_optimism(&parent_hash, &attrs, 3);
            world_chain_primitives::p2p::Authorization::new(
                payload_id,
                attrs.payload_attributes.timestamp,
                &authorizer_sk,
                builder_vk,
            )
        };

    // --- Flashblock stream: capture latest pending flashblock ---
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

    let mut driver = world_chain_test_utils::e2e_harness::actions::EngineDriver {
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
    use alloy_provider::Provider;
    use alloy_rpc_types::Filter;
    use world_chain_test_utils::e2e_harness::setup::encode_eip1559_params;

    reth_tracing::init_test_tracing();
    tokio::time::sleep(Duration::from_millis(100)).await;

    const NUM_BLOCKS: usize = 5;
    const BLOCK_INTERVAL: Duration = Duration::from_millis(2000);

    let (_, nodes, _tasks, mut env, tx_spammer) =
        setup::<WorldChainDefaultContext>(1, optimism_payload_attributes, true).await?;

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
        move |parent_hash: B256, attrs: reth_optimism_payload_builder::OpPayloadAttrs| {
            let authorizer_sk = ed25519_dalek::SigningKey::from_bytes(&[0; 32]);
            let payload_id =
                reth_optimism_payload_builder::payload_id_optimism(&parent_hash, &attrs, 3);
            world_chain_primitives::p2p::Authorization::new(
                payload_id,
                attrs.payload_attributes.timestamp,
                &authorizer_sk,
                builder_vk,
            )
        };

    let checks_passed = Arc::new(AtomicUsize::new(0));
    let checks_passed_cb = checks_passed.clone();

    let mut driver = world_chain_test_utils::e2e_harness::actions::EngineDriver {
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
        setup::<WorldChainDefaultContext>(1, optimism_payload_attributes, true).await?;

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
        move |parent_hash: B256, attrs: reth_optimism_payload_builder::OpPayloadAttrs| {
            let authorizer_sk = ed25519_dalek::SigningKey::from_bytes(&[0; 32]);
            let payload_id =
                reth_optimism_payload_builder::payload_id_optimism(&parent_hash, &attrs, 3);
            world_chain_primitives::p2p::Authorization::new(
                payload_id,
                attrs.payload_attributes.timestamp,
                &authorizer_sk,
                builder_vk,
            )
        };

    // Pre-compute assertions: expect the initial canon seed, then for each
    // block a base flashblock (index=0, is_base=true). Intermediate canon
    // events and delta flashblocks are skipped by the assertion checker.
    let mut assertions = Vec::new();

    // Initial canon tip seed from event_stream
    assertions.push(
        world_chain_test_utils::e2e_harness::actions::StreamAssertion::Canon { number: None },
    );

    // For each block, expect at least the base flashblock.
    // The checker skips intervening canon events automatically.
    for _ in 0..NUM_BLOCKS {
        assertions.push(
            world_chain_test_utils::e2e_harness::actions::StreamAssertion::Pending {
                index: 0,
                is_base: true,
            },
        );
    }

    // Subscribe to the event stream
    let stream = builder_context
        .flashblocks_handle
        .event_stream::<(), _, _, _>(
            nodes[0].node.inner.provider.clone(),
            |_: &WorldChainEvent<()>| None,
        );

    // Spawn assertion checker
    let assertion_handle =
        tokio::spawn(world_chain_test_utils::e2e_harness::actions::assert_stream(
            stream,
            assertions,
            Duration::from_secs(NUM_BLOCKS as u64 * 5),
        ));

    // Drive the engine
    let mut driver = world_chain_test_utils::e2e_harness::actions::EngineDriver {
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

#[tokio::test]
#[ignore = "Not correctly triggering payload building"]
async fn test_double_failover() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let (_, nodes, _tasks, _, _) =
        setup::<WorldChainDefaultContext>(3, optimism_payload_attributes, true).await?;

    let authorizer = SigningKey::from_bytes(&[0; 32]);

    let p2p_0 = nodes[0].ext_context.clone().unwrap().flashblocks_handle;
    let p2p_1 = nodes[1].ext_context.clone().unwrap().flashblocks_handle;
    let p2p_2 = nodes[2].ext_context.clone().unwrap().flashblocks_handle;

    let mut publish_flashblocks = p2p_0.ctx.flashblock_tx.subscribe();
    tokio::spawn(async move {
        while let Ok(payload) = publish_flashblocks.recv().await {
            println!("\n////////////////////////////////////////////////////////////////////\n");
            println!(
                "Received flashblock, payload_id: {}, index: {}",
                payload.payload_id, payload.index
            );
            println!("\n////////////////////////////////////////////////////////////////////\n");
        }
    });

    let provider = provider_from_url(nodes[0].node.rpc_url()).await;
    let latest_block = provider
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest)
        .await?
        .expect("latest block expected");
    assert_eq!(latest_block.number(), 0);

    let pending_block = provider
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Pending)
        .await?;
    assert_eq!(pending_block.unwrap().number(), latest_block.number());

    let payload_0 = base_payload(0, test_payload_id(10), 0, latest_block.hash(), AUTH_TS_BASE);
    let authorization_0 = Authorization::new(
        payload_0.payload_id,
        AUTH_TS_BASE,
        &authorizer,
        p2p_0.builder_sk()?.verifying_key(),
    );
    let msg = payload_0.clone();
    let authorized_0 = AuthorizedPayload::new(p2p_0.builder_sk()?, authorization_0, msg);
    p2p_0.start_publishing(authorization_0)?;
    p2p_0.publish_new(authorized_0).unwrap();
    sleep(Duration::from_millis(100)).await;

    let payload_1 = next_payload(payload_0.payload_id, 1).await;
    let authorization_1 = Authorization::new(
        payload_1.payload_id,
        AUTH_TS_BASE,
        &authorizer,
        p2p_1.builder_sk()?.verifying_key(),
    );
    let authorized_1 =
        AuthorizedPayload::new(p2p_1.builder_sk()?, authorization_1, payload_1.clone());
    p2p_1.start_publishing(authorization_1)?;
    sleep(Duration::from_millis(100)).await;
    p2p_1.publish_new(authorized_1).unwrap();
    sleep(Duration::from_millis(100)).await;

    let payload_2 = next_payload(payload_0.payload_id, 2).await;
    let msg = payload_2.clone();
    let authorization_2 = Authorization::new(
        payload_2.payload_id,
        AUTH_TS_BASE,
        &authorizer,
        p2p_2.builder_sk()?.verifying_key(),
    );
    let authorized_2 = AuthorizedPayload::new(p2p_2.builder_sk()?, authorization_2, msg.clone());
    p2p_2.start_publishing(authorization_2)?;
    sleep(Duration::from_millis(100)).await;
    p2p_2.publish_new(authorized_2).unwrap();
    sleep(Duration::from_millis(100)).await;

    Ok(())
}

#[tokio::test]
#[ignore = "Not correctly triggering payload building"]
async fn test_force_race_condition() -> eyre::Result<()> {
    let _tracing = init_tracing("warn,flashblocks=trace");

    let (_, nodes, _tasks, _, _) =
        setup::<WorldChainDefaultContext>(3, optimism_payload_attributes, true).await?;

    let authorizer = SigningKey::from_bytes(&[0; 32]);

    let p2p_0 = nodes[0].ext_context.clone().unwrap().flashblocks_handle;
    let p2p_1 = nodes[1].ext_context.clone().unwrap().flashblocks_handle;
    let p2p_2 = nodes[2].ext_context.clone().unwrap().flashblocks_handle;

    let mut publish_flashblocks = p2p_0.ctx.flashblock_tx.subscribe();
    tokio::spawn(async move {
        while let Ok(payload) = publish_flashblocks.recv().await {
            println!("\n////////////////////////////////////////////////////////////////////\n");
            println!(
                "Received flashblock, payload_id: {}, index: {}",
                payload.payload_id, payload.index
            );
            println!("\n////////////////////////////////////////////////////////////////////\n");
        }
    });

    let provider = provider_from_url(nodes[0].node.rpc_url()).await;
    let latest_block = provider
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest)
        .await?
        .expect("latest block expected");
    assert_eq!(latest_block.number(), 0);
    let expected_pending_number = latest_block.number() + 1;

    let pending_block = provider
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Pending)
        .await?;
    assert_eq!(pending_block.unwrap().number(), latest_block.number());

    let payload_0 = base_payload(0, test_payload_id(20), 0, latest_block.hash(), AUTH_TS_BASE);
    info!("Sending payload 0, index 0");
    let authorization = Authorization::new(
        payload_0.payload_id,
        AUTH_TS_BASE,
        &authorizer,
        p2p_0.builder_sk()?.verifying_key(),
    );
    let msg = payload_0.clone();
    let authorized = AuthorizedPayload::new(p2p_0.builder_sk()?, authorization, msg);
    p2p_0.start_publishing(authorization)?;
    p2p_0.publish_new(authorized).unwrap();

    p2p_wait_for_pending_block(nodes[0].node.rpc_url(), expected_pending_number, 0).await?;

    info!("Sending payload 0, index 1");
    let payload_1 = next_payload(payload_0.payload_id, 1).await;
    let authorization = Authorization::new(
        payload_1.payload_id,
        AUTH_TS_BASE,
        &authorizer,
        p2p_0.builder_sk()?.verifying_key(),
    );
    let authorized = AuthorizedPayload::new(p2p_0.builder_sk()?, authorization, payload_1.clone());
    p2p_0.publish_new(authorized).unwrap();

    p2p_wait_for_pending_block(nodes[0].node.rpc_url(), expected_pending_number, 0).await?;

    let payload_2 = base_payload(1, test_payload_id(21), 0, latest_block.hash(), AUTH_TS_NEXT);
    info!("Sending payload 1, index 0");
    let authorization_1 = Authorization::new(
        payload_2.payload_id,
        AUTH_TS_NEXT,
        &authorizer,
        p2p_1.builder_sk()?.verifying_key(),
    );
    let authorization_2 = Authorization::new(
        payload_2.payload_id,
        AUTH_TS_NEXT,
        &authorizer,
        p2p_2.builder_sk()?.verifying_key(),
    );
    let msg = payload_2.clone();
    let authorized_1 = AuthorizedPayload::new(p2p_1.builder_sk()?, authorization_1, msg.clone());
    p2p_1.start_publishing(authorization_1)?;
    p2p_2.start_publishing(authorization_2)?;
    sleep(Duration::from_millis(100)).await;
    tracing::error!("{}", p2p_1.publish_new(authorized_1.clone()).unwrap_err());
    sleep(Duration::from_millis(100)).await;

    p2p_2.stop_publishing()?;
    sleep(Duration::from_millis(100)).await;

    p2p_1.publish_new(authorized_1)?;
    sleep(Duration::from_millis(100)).await;

    Ok(())
}

#[tokio::test]
async fn test_receive_peer_latency_scores_are_recorded() -> eyre::Result<()> {
    let _tracing = init_tracing("warn,flashblocks=trace");

    let (_, nodes, _tasks, _, _) =
        setup::<WorldChainDefaultContext>(3, optimism_payload_attributes, true).await?;

    let authorizer = SigningKey::from_bytes(&[0; 32]);
    let p2p_0 = nodes[0].ext_context.clone().unwrap().flashblocks_handle;

    let (receive_peers, candidate_peers) = wait_for_flashblocks_topology(&p2p_0, 2, 2).await?;
    assert!(
        candidate_peers.is_empty(),
        "expected no spare candidate peer"
    );

    let slow_peer = receive_peers[0];
    let fast_peer = receive_peers[1];

    let peer_map: HashMap<_, _> = nodes
        .iter()
        .skip(1)
        .map(|node| {
            let peer_id = node.node.network.record().id;
            let p2p = node.ext_context.clone().unwrap().flashblocks_handle;
            let rpc_url = node.node.rpc_url();
            (peer_id, (p2p, rpc_url))
        })
        .collect();

    let (fast_p2p, fast_rpc) = peer_map
        .get(&fast_peer)
        .expect("fast peer should map to a node");
    let (slow_p2p, slow_rpc) = peer_map
        .get(&slow_peer)
        .expect("slow peer should map to a node");

    for (payload_suffix, authorization_timestamp) in [(41, 41_u64), (42, 42), (43, 43), (44, 44)] {
        publish_flashblock_with_latency(
            fast_p2p,
            fast_rpc.clone(),
            &authorizer,
            test_payload_id(payload_suffix),
            authorization_timestamp,
            Duration::from_millis(10),
        )
        .await?;
        sleep(Duration::from_millis(50)).await;

        publish_flashblock_with_latency(
            slow_p2p,
            slow_rpc.clone(),
            &authorizer,
            test_payload_id(payload_suffix + 10),
            authorization_timestamp + 10,
            Duration::from_millis(300),
        )
        .await?;
        sleep(Duration::from_millis(50)).await;
    }

    sleep(Duration::from_millis(250)).await;

    let timeout = Duration::from_secs(15);
    let poll_interval = Duration::from_millis(100);
    let start = Instant::now();

    loop {
        let fast_score = receive_peer_score(&p2p_0, fast_peer)?;
        let slow_score = receive_peer_score(&p2p_0, slow_peer)?;

        if fast_score.is_some() && slow_score.is_some() {
            break;
        }

        if start.elapsed() >= timeout {
            return Err(eyre!(
                "timed out waiting for latency scores to be recorded: fast={fast_peer} fast_score={fast_score:?}, slow={slow_peer} slow_score={slow_score:?}"
            ));
        }

        sleep(poll_interval).await;
    }

    Ok(())
}

#[tokio::test]
#[ignore = "Not correctly triggering payload building"]
async fn test_get_block_by_number_pending() -> eyre::Result<()> {
    let _tracing = init_tracing("warn,flashblocks=trace");

    let (_, nodes, _tasks, _, _) =
        setup::<WorldChainDefaultContext>(1, optimism_payload_attributes, true).await?;

    let authorizer = SigningKey::from_bytes(&[0; 32]);
    let p2p_0 = nodes[0].ext_context.clone().unwrap().flashblocks_handle;

    let provider = provider_from_url(nodes[0].node.rpc_url()).await;

    let latest_block = provider
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest)
        .await?
        .expect("latest block expected");
    assert_eq!(latest_block.number(), 0);
    let expected_pending_number = latest_block.number() + 1;

    let pending_block = provider
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Pending)
        .await?;
    assert_eq!(pending_block.unwrap().number(), latest_block.number());

    let payload_id = test_payload_id(30);
    let base = base_payload(0, payload_id, 0, latest_block.hash(), AUTH_TS_BASE);
    let authorization = Authorization::new(
        base.payload_id,
        AUTH_TS_BASE,
        &authorizer,
        p2p_0.builder_sk()?.verifying_key(),
    );
    let authorized = AuthorizedPayload::new(p2p_0.builder_sk()?, authorization, base);
    p2p_0.start_publishing(authorization)?;
    p2p_0.publish_new(authorized).unwrap();

    p2p_wait_for_pending_block(nodes[0].node.rpc_url(), expected_pending_number, 0).await?;

    let next = next_payload(payload_id, 1).await;
    let authorization = Authorization::new(
        next.payload_id,
        AUTH_TS_BASE,
        &authorizer,
        p2p_0.builder_sk()?.verifying_key(),
    );
    let authorized = AuthorizedPayload::new(p2p_0.builder_sk()?, authorization, next);
    p2p_0.start_publishing(authorization)?;
    p2p_0.publish_new(authorized).unwrap();

    p2p_wait_for_pending_block(nodes[0].node.rpc_url(), expected_pending_number, 0).await?;

    Ok(())
}

#[tokio::test]
async fn test_peer_reputation() -> eyre::Result<()> {
    let _tracing = init_tracing("warn,flashblocks=trace");

    let (_, nodes, _tasks, _, _) =
        setup::<WorldChainDefaultContext>(2, optimism_payload_attributes, true).await?;

    let p2p_0 = nodes[0].ext_context.clone().unwrap().flashblocks_handle;
    let network_1 = &nodes[1].node.inner.network;

    let invalid_authorizer = SigningKey::from_bytes(&[99; 32]);
    let provider = provider_from_url(nodes[0].node.rpc_url()).await;
    let latest_block = provider
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest)
        .await?
        .expect("latest block expected");

    let payload_0 = base_payload(0, test_payload_id(40), 0, latest_block.hash(), AUTH_TS_BASE);
    info!("Sending bad authorization");
    let authorization = Authorization::new(
        payload_0.payload_id,
        AUTH_TS_BASE,
        &invalid_authorizer,
        p2p_0.builder_sk()?.verifying_key(),
    );

    let authorized_msg = AuthorizedMsg::StartPublish(StartPublish);
    let authorized_payload = Authorized::new(p2p_0.builder_sk()?, authorization, authorized_msg);
    let p2p_msg = FlashblocksP2PMsg::Authorized(authorized_payload);
    let bytes = p2p_msg.encode();

    let peers = network_1.get_all_peers().await?;
    let peer_0 = &peers[0].remote_id;

    let mut reputation_was_negative = false;
    let mut peer_banned = false;
    for _ in 0..100 {
        p2p_0.send_serialized_to_all_peers(bytes.clone());
        sleep(Duration::from_millis(10)).await;
        let rep_0 = network_1.reputation_by_id(*peer_0).await?;
        if let Some(rep) = rep_0
            && rep < 0
        {
            reputation_was_negative = true;
        }

        if network_1.get_all_peers().await?.is_empty() {
            peer_banned = true;
            break;
        }
    }

    assert!(
        reputation_was_negative,
        "Peer reputation should have become negative"
    );
    assert!(peer_banned, "Peer should have been banned");

    Ok(())
}

#[ignore]
#[tokio::test]
async fn test_peer_monitoring() -> eyre::Result<()> {
    use reth_e2e_test_utils::TmpDB;
    use reth_network_peers::TrustedPeer;
    use reth_node_api::{FullNodeTypesAdapter, NodeTypesWithDBAdapter};
    use reth_node_builder::{Node, NodeBuilder, NodeConfig, NodeHandle};
    use reth_node_core::args::{DiscoveryArgs, NetworkArgs, RpcServerArgs};
    use reth_provider::providers::BlockchainProvider;
    use reth_tasks::TaskExecutor;
    use std::{
        net::{IpAddr, SocketAddr},
        path::PathBuf,
    };
    use tracing_subscriber::layer::SubscriberExt;
    use url::Host;
    use world_chain_cli::{BuilderArgs, PbhArgs, WorldChainArgs};
    use world_chain_node::node::WorldChainNode;
    use world_chain_test_utils::{DEV_WORLD_ID, PBH_DEV_ENTRYPOINT, PBH_DEV_SIGNATURE_AGGREGATOR};

    /// Local setup for the peer monitoring test that needs per-node control
    /// over port, P2P key, and task executor.
    async fn setup_monitoring_node(
        exec: TaskExecutor,
        authorizer_sk: SigningKey,
        builder_sk: SigningKey,
        peers: Vec<(PeerId, SocketAddr)>,
        port: Option<u16>,
        p2p_secret_key: Option<PathBuf>,
    ) -> eyre::Result<(
        world_chain_p2p::protocol::handler::FlashblocksHandle,
        reth_network_peers::NodeRecord,
        reth_network::NetworkHandle<
            reth_eth_wire::BasicNetworkPrimitives<
                reth_optimism_primitives::OpPrimitives,
                op_alloy_consensus::OpPooledTransaction,
                reth_network::types::NewBlock<
                    alloy_consensus::Block<op_alloy_consensus::OpTxEnvelope>,
                >,
            >,
        >,
        reth_node_core::exit::NodeExitFuture,
        Box<dyn std::any::Any + Sync + Send>,
    )> {
        let op_chain_spec: Arc<reth_optimism_chainspec::OpChainSpec> = Arc::new(CHAIN_SPEC.clone());

        let mut network_config = NetworkArgs {
            discovery: DiscoveryArgs {
                disable_discovery: true,
                ..DiscoveryArgs::default()
            },
            ..NetworkArgs::default()
        };

        if let Some(p) = port {
            network_config.port = p;
        }
        if let Some(key_path) = p2p_secret_key {
            network_config.p2p_secret_key = Some(key_path);
        }

        network_config.trusted_peers = peers
            .into_iter()
            .map(|(peer_id, addr)| {
                let host = match addr.ip() {
                    IpAddr::V4(ip) => Host::Ipv4(ip),
                    IpAddr::V6(ip) => Host::Ipv6(ip),
                };
                TrustedPeer::new(host, addr.port(), peer_id)
            })
            .collect();

        let mut node_config = NodeConfig::new(op_chain_spec.clone())
            .with_chain(op_chain_spec)
            .with_network(network_config)
            .with_rpc(RpcServerArgs::default().with_unused_ports().with_http());

        if port.is_none() {
            node_config = node_config.with_unused_ports();
        }

        let pbh = PbhArgs {
            verified_blockspace_capacity: 70,
            entrypoint: PBH_DEV_ENTRYPOINT,
            signature_aggregator: PBH_DEV_SIGNATURE_AGGREGATOR,
            world_id: DEV_WORLD_ID,
        };

        let builder = BuilderArgs {
            enabled: false,
            private_key: world_chain_test_utils::utils::signer(6),
            block_uncompressed_size_limit: None,
        };

        let args = WorldChainArgs {
            rollup: Default::default(),
            builder,
            pbh,
            flashblocks: Some(test_flashblocks_args(&authorizer_sk, &builder_sk)),
            tx_peers: None,
            disable_bootnodes: true,
        };

        let wc_config = args.clone().into_config(&mut node_config)?;

        let node = WorldChainNode::<WorldChainDefaultContext>::new(wc_config);

        let ext_context = node.ext_context::<FullNodeTypesAdapter<
            WorldChainNode<WorldChainDefaultContext>,
            TmpDB,
            BlockchainProvider<
                NodeTypesWithDBAdapter<WorldChainNode<WorldChainDefaultContext>, TmpDB>,
            >,
        >>();
        let p2p_handle = ext_context.unwrap().flashblocks_handle.clone();

        let NodeHandle {
            node,
            node_exit_future,
        } = NodeBuilder::new(node_config)
            .testing_node(exec)
            .with_types_and_provider::<WorldChainNode<WorldChainDefaultContext>, BlockchainProvider<_>>()
            .with_components(node.components_builder())
            .with_add_ons(node.add_ons())
            .launch()
            .await?;

        let network_handle = node.network.clone();
        let local_node_record = network_handle.local_node_record();

        Ok((
            p2p_handle,
            local_node_record,
            network_handle,
            node_exit_future,
            Box::new(node),
        ))
    }

    let log_buffer = SharedLogBuffer::default();
    let subscriber = tracing_subscriber::registry().with(log_buffer.clone());
    tracing::subscriber::set_global_default(subscriber).expect("failed to set global subscriber");

    let authorizer = SigningKey::from_bytes(&[0; 32]);

    let mut p2p_key_file = NamedTempFile::new()?;
    p2p_key_file.write_all(b"0101010101010101010101010101010101010101010101010101010101010101")?;
    p2p_key_file.flush()?;
    let p2p_key_path = p2p_key_file.path().to_path_buf();

    let exec1 = Runtime::test();

    let builder1 = SigningKey::from_bytes(&[1; 32]);
    let (p2p_1, record_1, network_1, _exit_1, _node_1) = setup_monitoring_node(
        exec1.clone(),
        authorizer.clone(),
        builder1,
        vec![],
        None,
        Some(p2p_key_path.clone()),
    )
    .await?;

    let exec2 = Runtime::test();

    let peer1_id = record_1.id;
    let peer1_addr = record_1.tcp_addr();
    let peer1_port = peer1_addr.port();

    let builder2 = SigningKey::from_bytes(&[2; 32]);
    let (_p2p_2, record_2, network_2, _exit_2, _node_2) = setup_monitoring_node(
        exec2.clone(),
        authorizer.clone(),
        builder2,
        vec![(peer1_id, peer1_addr)],
        None,
        None,
    )
    .await?;

    let start = Instant::now();
    loop {
        let trusted_peers = network_2.get_trusted_peers().await?;
        if trusted_peers.len() == 1 {
            info!(
                "Connection established in {:.2}s",
                start.elapsed().as_secs_f64()
            );
            break;
        }
        if start.elapsed() > Duration::from_secs(10) {
            panic!("Timeout waiting for connection to establish");
        }
        sleep(Duration::from_millis(100)).await;
    }

    sleep(monitor::PEER_MONITOR_INTERVAL + Duration::from_secs(1)).await;

    info!("Simulating node 1 crash (dropping node and TaskManager)");
    drop(_node_1);
    drop(_exit_1);
    drop(p2p_1);
    drop(network_1);

    sleep(Duration::from_millis(500)).await;

    {
        let logs = log_buffer.logs();
        let disconnect_log_exists = logs.iter().any(|log| {
            log.contains("trusted peer disconnected") && log.contains(&peer1_id.to_string())
        });
        assert!(
            disconnect_log_exists,
            "Should have logged 'trusted peer disconnected' for peer {} from event listener",
            peer1_id
        );
    }

    sleep(monitor::PEER_MONITOR_INTERVAL * 2 + Duration::from_secs(1)).await;

    let trusted_peers_after = network_2.get_trusted_peers().await?;
    assert_eq!(
        trusted_peers_after.len(),
        0,
        "Node 1 should have disconnected"
    );

    sleep(Duration::from_secs(2)).await;

    info!("Testing peer reconnection after restart");
    info!("Restarting node 1 with the same port and P2P key");

    let (_, record_1_new, _, _exit_1_new, _node_1_new) = setup_monitoring_node(
        exec2.clone(),
        authorizer.clone(),
        SigningKey::from_bytes(&[1; 32]),
        vec![(record_2.id, record_2.tcp_addr())],
        Some(peer1_port),
        Some(p2p_key_path.clone()),
    )
    .await?;
    let peer1_id_new = record_1_new.id;
    let peer1_addr_new = record_1_new.tcp_addr();

    assert_eq!(
        peer1_addr, peer1_addr_new,
        "Peer address should remain the same after restart (fixed port)"
    );
    assert_eq!(
        peer1_id, peer1_id_new,
        "Peer ID should remain the same after restart (reused P2P key)"
    );

    info!(
        "Restarted peer with same ID and address: peer_id={}, addr={}",
        peer1_id_new, peer1_addr_new
    );

    let connection_timeout = Duration::from_secs(10);
    let poll_interval = Duration::from_millis(100);
    let start = Instant::now();

    loop {
        let trusted_peers = network_2.get_trusted_peers().await?;
        if trusted_peers.len() == 1 {
            info!(
                "Reconnection established in {:.2}s",
                start.elapsed().as_secs_f64()
            );
            break;
        }
        if start.elapsed() > connection_timeout {
            panic!("Timeout waiting for reconnection to establish");
        }
        sleep(poll_interval).await;
    }

    {
        let logs = log_buffer.logs();
        let reconnection_log_exists = logs.iter().any(|log| {
            log.contains("connection to trusted peer established")
                && log.contains(&peer1_id.to_string())
        });
        assert!(
            reconnection_log_exists,
            "Should have logged 'connection to trusted peer established' for peer {} after restart",
            peer1_id
        );
    }

    sleep(monitor::PEER_MONITOR_INTERVAL + Duration::from_secs(1)).await;

    {
        let logs = log_buffer.logs();
        let reconnection_log_idx = logs
            .iter()
            .rposition(|log| log.contains("connection to trusted peer established"))
            .expect("Could not find 'connection to trusted peer established' log");

        let (logs_before_reconnect, logs_after_reconnect) = logs.split_at(reconnection_log_idx);

        let warnings_before_reconnect: Vec<&String> = logs_before_reconnect
            .iter()
            .filter(|log| {
                log.contains(&peer1_id.to_string())
                    && log.contains("WARN")
                    && log.contains("trusted peer disconnected")
            })
            .collect();

        assert!(
            warnings_before_reconnect.len() >= 2,
            "Should have had at least 2 warnings before reconnection, found {}",
            warnings_before_reconnect.len()
        );

        let warnings_after_reconnect: Vec<&String> = logs_after_reconnect
            .iter()
            .filter(|log| {
                log.contains(&peer1_id.to_string())
                    && log.contains("WARN")
                    && log.contains("trusted peer disconnected")
            })
            .collect();

        assert!(
            warnings_after_reconnect.is_empty(),
            "Should have no warnings after reconnection, found {}: {:?}",
            warnings_after_reconnect.len(),
            warnings_after_reconnect
        );
    }

    Ok(())
}

/// Thread-safe log buffer for capturing tracing output across threads.
#[derive(Clone, Default)]
struct SharedLogBuffer(Arc<std::sync::Mutex<Vec<String>>>);

impl SharedLogBuffer {
    fn logs(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SharedLogBuffer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = LogVisitor(String::new());
        visitor
            .0
            .push_str(&format!("{} ", event.metadata().level()));
        visitor
            .0
            .push_str(&format!("{}: ", event.metadata().target()));
        event.record(&mut visitor);
        self.0.lock().unwrap().push(visitor.0);
    }
}

struct LogVisitor(String);

impl tracing::field::Visit for LogVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.0.push_str(&format!("{:?}", value));
        } else {
            self.0.push_str(&format!(" {}={:?}", field.name(), value));
        }
    }
}

const AUTH_TS_BASE: u64 = 1;
const AUTH_TS_NEXT: u64 = 2;

fn test_payload_id(id: u8) -> PayloadId {
    PayloadId::new([id; 8])
}

fn init_tracing(filter: &str) -> tracing::subscriber::DefaultGuard {
    let sub = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_target(false)
        .without_time()
        .finish();

    Dispatch::new(sub).set_default()
}

fn test_flashblocks_args(authorizer_sk: &SigningKey, builder_sk: &SigningKey) -> FlashblocksArgs {
    FlashblocksArgs {
        enabled: true,
        authorizer_vk: Some(authorizer_sk.verifying_key()),
        builder_sk: Some(builder_sk.clone()),
        force_publish: false,
        override_authorizer_sk: None,
        flashblocks_interval: 200,
        recommit_interval: 200,
        access_list: true,
        fanout: Default::default(),
    }
}

async fn wait_for_flashblocks_topology(
    p2p_handle: &world_chain_p2p::protocol::handler::FlashblocksHandle,
    expected_connections: usize,
    expected_receive_peers: usize,
) -> eyre::Result<(Vec<PeerId>, Vec<PeerId>)> {
    let timeout = Duration::from_secs(10);
    let poll_interval = Duration::from_millis(100);
    let start = Instant::now();

    loop {
        {
            let state = p2p_handle.state.lock();
            if state.peers.len() == expected_connections {
                let receive_peers: Vec<_> = state
                    .peers
                    .iter()
                    .filter_map(|(peer_id, conn)| {
                        matches!(conn.receive_status, ReceiveStatus::Receiving { .. })
                            .then_some(*peer_id)
                    })
                    .collect();
                let candidate_peers: Vec<_> = state
                    .peers
                    .iter()
                    .filter_map(|(peer_id, conn)| {
                        (conn.receive_status == ReceiveStatus::NotReceiving).then_some(*peer_id)
                    })
                    .collect();
                drop(state);

                if receive_peers.len() == expected_receive_peers
                    && receive_peers.len() + candidate_peers.len() == expected_connections
                {
                    return Ok((receive_peers, candidate_peers));
                }
            } else {
                drop(state);
            }

            if start.elapsed() >= timeout {
                return Err(eyre!(
                    "timed out waiting for flashblocks topology: expected {expected_connections} connections with {expected_receive_peers} receive peers"
                ));
            }
        }

        sleep(poll_interval).await;
    }
}

fn receive_peer_score(
    p2p_handle: &world_chain_p2p::protocol::handler::FlashblocksHandle,
    peer_id: PeerId,
) -> eyre::Result<Option<i64>> {
    let state = p2p_handle.state.lock();
    let peer = state
        .peers
        .get(&peer_id)
        .ok_or_else(|| eyre!("peer {peer_id} not found"))?;

    let ReceiveStatus::Receiving { score } = &peer.receive_status else {
        return Ok(None);
    };

    let debug = format!("{score:?}");
    let value = debug
        .split("value: ")
        .nth(1)
        .and_then(|rest| rest.strip_prefix("Some("))
        .and_then(|rest| rest.split(')').next())
        .ok_or_else(|| eyre!("failed to parse score from {debug}"))?
        .parse::<i64>()?;

    Ok(Some(value))
}

fn base_payload(
    block_number: u64,
    payload_id: PayloadId,
    index: u64,
    hash: B256,
    timestamp: u64,
) -> FlashblocksPayloadV1 {
    FlashblocksPayloadV1 {
        payload_id,
        index,
        base: Some(ExecutionPayloadBaseV1 {
            parent_beacon_block_root: B256::default(),
            parent_hash: hash,
            fee_recipient: Address::ZERO,
            prev_randao: B256::default(),
            block_number,
            gas_limit: 0,
            timestamp,
            extra_data: Bytes::new(),
            base_fee_per_gas: U256::ZERO,
        }),
        metadata: FlashblockMetadata::default(),
        diff: ExecutionPayloadFlashblockDeltaV1::default(),
    }
}

async fn next_payload(payload_id: PayloadId, index: u64) -> FlashblocksPayloadV1 {
    let tx1 = raw_tx(
        0,
        eip1559()
            .chain_id(8453)
            .nonce(0)
            .max_fee_per_gas(2_000_000_000u128)
            .max_priority_fee_per_gas(100_000_000u128)
            .to(account(1))
            .call(),
    )
    .await;
    let tx2 = raw_tx(
        0,
        eip1559()
            .chain_id(8453)
            .nonce(1)
            .max_fee_per_gas(2_000_000_000u128)
            .max_priority_fee_per_gas(100_000_000u128)
            .to(account(2))
            .call(),
    )
    .await;

    FlashblocksPayloadV1 {
        payload_id,
        index,
        base: None,
        diff: ExecutionPayloadFlashblockDeltaV1 {
            state_root: B256::default(),
            receipts_root: B256::default(),
            gas_used: 0,
            block_hash: B256::default(),
            transactions: vec![tx1, tx2],
            withdrawals: Vec::new(),
            logs_bloom: Default::default(),
            withdrawals_root: Default::default(),
            access_list_data: Default::default(),
        },
        metadata: FlashblockMetadata::default(),
    }
}

async fn publish_flashblock_with_latency(
    p2p_handle: &world_chain_p2p::protocol::handler::FlashblocksHandle,
    rpc_url: url::Url,
    authorizer: &SigningKey,
    payload_id: PayloadId,
    authorization_timestamp: u64,
    simulated_latency: Duration,
) -> eyre::Result<()> {
    let provider = provider_from_url(rpc_url).await;
    let latest_block = provider
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest)
        .await?
        .expect("latest block expected");
    let mut payload = base_payload(
        0,
        payload_id,
        0,
        latest_block.hash(),
        authorization_timestamp,
    );
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_nanos() as i64;
    payload.metadata.flashblock_timestamp = Some(now - simulated_latency.as_nanos() as i64);
    let authorization = Authorization::new(
        payload.payload_id,
        authorization_timestamp,
        authorizer,
        p2p_handle.builder_sk()?.verifying_key(),
    );
    let authorized = AuthorizedPayload::new(p2p_handle.builder_sk()?, authorization, payload);

    {
        let state = p2p_handle.state.lock();
        state
            .publishing_status
            .send_replace(PublishingStatus::Publishing { authorization });
    }
    p2p_handle.publish_new(authorized)?;
    {
        let state = p2p_handle.state.lock();
        state
            .publishing_status
            .send_replace(PublishingStatus::NotPublishing {
                active_publishers: Vec::new(),
            });
    }

    Ok(())
}

async fn provider_from_url(url: url::Url) -> RootProvider {
    let client = RpcClient::builder().http(url);
    RootProvider::new(client)
}

async fn p2p_wait_for_pending_block(
    rpc_url: url::Url,
    expected_number: u64,
    expected_txs: usize,
) -> eyre::Result<()> {
    let provider = provider_from_url(rpc_url).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = String::new();

    loop {
        let pending = provider
            .get_block_by_number(alloy_eips::BlockNumberOrTag::Pending)
            .await?;

        if let Some(ref block) = pending {
            if block.number() == expected_number
                && block.transactions.hashes().len() == expected_txs
            {
                return Ok(());
            }
            last = format!(
                "number={}, txs={}",
                block.number(),
                block.transactions.hashes().len()
            );
        }

        if Instant::now() >= deadline {
            return Err(eyre!(
                "timed out waiting for pending block: expected number={expected_number} txs={expected_txs}, last observed: {last}"
            ));
        }

        sleep(Duration::from_millis(50)).await;
    }
}
/// Test that the `OpBuiltPayload` produced by the coordinator (consuming flashblocks)
/// matches the payload built by the payload builder on the builder node.
///
/// Flow:
/// 1. Builder mines a block with flashblocks enabled, producing flashblocks over P2P
/// 2. Follower processes flashblocks through the coordinator (`validate_flashblock_with_state`)
/// 3. After mining, the builder's `get_payload_v4` result and the follower's coordinator
///    pending block should agree on block_hash, state_root, receipts_root, and gas_used.
#[tokio::test(flavor = "multi_thread")]
async fn test_coordinator_payload_matches_builder() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    const TRANSACTIONS_PER_FLASHBLOCK: u64 = 10;

    // Builder (node 0) and Follower (node 1) with flashblocks enabled
    let (_, mut nodes, _tasks, mut env, tx_spammer) =
        setup::<WorldChainDefaultContext>(2, optimism_payload_attributes, true).await?;

    let [builder_node, follower_node] = &mut nodes[..] else {
        unreachable!()
    };

    let builder_context = builder_node.ext_context.clone().unwrap();
    let follower_context = follower_node.ext_context.clone().unwrap();

    let rpc_url = builder_node.node.rpc_url();
    tx_spammer.spawn(TRANSACTIONS_PER_FLASHBLOCK, rpc_url);

    let block_hash = builder_node.node.block_hash(0);

    let authorization_generator =
        world_chain_test_utils::e2e_harness::setup::create_authorization_generator(
            block_hash,
            builder_context
                .flashblocks_handle
                .builder_sk()
                .unwrap()
                .verifying_key(),
        );

    let timestamp = world_chain_test_utils::e2e_harness::setup::current_timestamp();
    let eip1559_params = world_chain_test_utils::e2e_harness::setup::encode_eip1559_params(
        builder_node.node.inner.chain_spec().as_ref(),
        timestamp,
    )?;

    let attributes = world_chain_test_utils::e2e_harness::setup::build_payload_attributes(
        timestamp,
        eip1559_params,
        Some(vec![
            world_chain_test_utils::e2e_harness::setup::TX_SET_L1_BLOCK.clone(),
        ]),
    );

    // Mine a block on the builder — this produces flashblocks that the follower processes
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);

    let mine_block = world_chain_test_utils::e2e_harness::actions::AssertMineBlock::new(
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

    // Wait for mining to complete and get the builder's payload envelope
    let builder_envelope = rx
        .recv()
        .await
        .ok_or(eyre!("failed to receive mined block from builder"))?;

    let builder_block_hash = builder_envelope
        .execution_payload
        .payload_inner
        .payload_inner
        .payload_inner
        .block_hash;
    let builder_state_root = builder_envelope
        .execution_payload
        .payload_inner
        .payload_inner
        .payload_inner
        .state_root;
    let builder_receipts_root = builder_envelope
        .execution_payload
        .payload_inner
        .payload_inner
        .payload_inner
        .receipts_root;
    let builder_gas_used = builder_envelope
        .execution_payload
        .payload_inner
        .payload_inner
        .payload_inner
        .gas_used;
    let builder_tx_count = builder_envelope
        .execution_payload
        .payload_inner
        .payload_inner
        .payload_inner
        .transactions
        .len();

    info!(
        target: "test",
        ?builder_block_hash,
        ?builder_state_root,
        ?builder_receipts_root,
        builder_gas_used,
        builder_tx_count,
        "Builder produced final payload"
    );

    // Give the follower time to process the final flashblock
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Get the follower's coordinator-built pending block
    let pending_block = follower_context.flashblocks_state.pending_block();
    let coordinator_block = pending_block
        .borrow()
        .clone()
        .ok_or(eyre!("Follower has no pending block from coordinator"))?;

    let coord_block = coordinator_block.recovered_block();
    let coord_block_hash = coord_block.hash();
    let coord_state_root = coord_block.state_root();
    let coord_receipts_root = coord_block.receipts_root();
    let coord_gas_used = coord_block.gas_used();
    let coord_tx_count = coord_block.body().transactions.len();

    info!(
        target: "test",
        ?coord_block_hash,
        ?coord_state_root,
        ?coord_receipts_root,
        coord_gas_used,
        coord_tx_count,
        "Coordinator produced pending block"
    );

    // Assert the coordinator's payload matches the builder's payload
    assert_eq!(
        builder_block_hash, coord_block_hash,
        "Block hash mismatch between builder and coordinator"
    );
    assert_eq!(
        builder_state_root, coord_state_root,
        "State root mismatch between builder and coordinator"
    );
    assert_eq!(
        builder_receipts_root, coord_receipts_root,
        "Receipts root mismatch between builder and coordinator"
    );
    assert_eq!(
        builder_gas_used, coord_gas_used,
        "Gas used mismatch between builder and coordinator"
    );
    assert_eq!(
        builder_tx_count, coord_tx_count,
        "Transaction count mismatch between builder and coordinator"
    );

    Ok(())
}
