//! Utilities for running world chain builder end-to-end tests.
use crate::args::WorldChainArgs;
use crate::flashblocks::WorldChainFlashblocksNode;
use crate::test_utils::tx;
use crate::tests::{get_chain_spec, WorldChainNode, BASE_CHAIN_ID};
use alloy_network::eip2718::Encodable2718;
use alloy_primitives::Bytes;
use flashblocks::args::FlashblockArgs;
use futures::StreamExt;
use op_alloy_consensus::OpTxEnvelope;
use reth::api::TreeConfig;
use reth::args::PayloadBuilderArgs;
use reth::builder::Node;
use reth::builder::{EngineNodeLauncher, NodeBuilder, NodeConfig, NodeHandle};
use reth::tasks::TaskManager;
use reth_e2e_test_utils::node::NodeTestContext;
use reth_e2e_test_utils::transaction::TransactionTestContext;
use reth_node_api::{BlockBody, PayloadBuilderAttributes};
use reth_node_core::args::RpcServerArgs;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::utils::optimism_payload_attributes;
use reth_provider::providers::BlockchainProvider;
use revm_primitives::Address;
use rollup_boost::FlashblocksPayloadV1;
use std::collections::HashSet;
use std::ops::Range;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{info, span, warn};
use world_chain_builder_rpc::{EthApiExtServer, WorldChainEthApiExt};
use world_chain_builder_test_utils::utils::signer;
use world_chain_builder_test_utils::{
    DEV_WORLD_ID, PBH_DEV_ENTRYPOINT, PBH_DEV_SIGNATURE_AGGREGATOR,
};

pub async fn setup_swarm(
    num_nodes: u8,
) -> eyre::Result<(
    Range<u8>,
    Vec<WorldChainNode<WorldChainFlashblocksNode>>,
    TaskManager,
)> {
    std::env::set_var("PRIVATE_KEY", DEV_WORLD_ID.to_string());
    let op_chain_spec = Arc::new(get_chain_spec());

    let task_manager = TaskManager::current();
    let exec = task_manager.executor();

    let mut node_config: NodeConfig<OpChainSpec> = NodeConfig::new(op_chain_spec.clone())
        .with_chain(op_chain_spec.clone())
        .with_rpc(
            RpcServerArgs::default()
                .with_unused_ports()
                .with_http_unused_port(),
        )
        .with_payload_builder(PayloadBuilderArgs {
            deadline: Duration::from_millis(4000),
            max_payload_tasks: 1,
            gas_limit: Some(35_000_000),
            interval: Duration::from_millis(4000),
            ..Default::default()
        })
        .with_unused_ports();

    // discv5 ports seem to be clashing
    node_config.network.discovery.disable_discovery = true;
    node_config.network.discovery.addr = [127, 0, 0, 1].into();

    // is 0.0.0.0 by default
    node_config.network.addr = [127, 0, 0, 1].into();

    let mut nodes: Vec<_> =
        Vec::<WorldChainNode<WorldChainFlashblocksNode>>::with_capacity(num_nodes as usize);

    for idx in 0..num_nodes {
        let span = span!(tracing::Level::INFO, "test_node", idx);

        let _enter = span.enter();

        let node = WorldChainFlashblocksNode::new(WorldChainArgs {
            verified_blockspace_capacity: 70,
            pbh_entrypoint: PBH_DEV_ENTRYPOINT,
            signature_aggregator: PBH_DEV_SIGNATURE_AGGREGATOR,
            world_id: DEV_WORLD_ID,
            builder_private_key: signer(6).to_bytes().to_string(),
            flashblock_args: Some(FlashblockArgs {
                flashblock_port: 9002 + idx as u16,
                ..Default::default()
            }),
            ..Default::default()
        });

        let NodeHandle {
            node,
            node_exit_future: _,
        } = NodeBuilder::new(node_config.clone())
            .testing_node(exec.clone())
            .with_types_and_provider::<WorldChainFlashblocksNode, BlockchainProvider<_>>()
            .with_components(node.components_builder())
            .with_add_ons(node.add_ons())
            .extend_rpc_modules(move |ctx| {
                let provider = ctx.provider().clone();
                let pool = ctx.pool().clone();
                let eth_api_ext = WorldChainEthApiExt::new(pool, provider, None);

                ctx.modules.replace_configured(eth_api_ext.into_rpc())?;
                Ok(())
            })
            .launch_with_fn(|builder| {
                let launcher = EngineNodeLauncher::new(
                    builder.task_executor().clone(),
                    builder.config().datadir(),
                    TreeConfig::default(),
                );
                builder.launch_with(launcher)
            })
            .await?;

        let mut node =
            NodeTestContext::new(node, optimism_payload_attributes::<OpTxEnvelope>).await?;

        // Connect each node in a chain.
        if let Some(previous_node) = nodes.last_mut() {
            previous_node.connect(&mut node).await;
        }

        // Connect last node with the first if there are more than two
        if idx + 1 == num_nodes && num_nodes > 2 {
            if let Some(first_node) = nodes.first_mut() {
                node.connect(first_node).await;
            }
        }

        nodes.push(node);
    }

    Ok((0..6, nodes, task_manager))
}

#[tokio::test(flavor = "multi_thread")]
async fn test_build_flashblocks() -> eyre::Result<()> {
    tokio::time::sleep(Duration::from_secs(1)).await;
    reth_tracing::init_test_tracing();
    let (_signers, mut nodes, _task_manager) = setup_swarm(3).await?;

    let test_ctx = nodes.first_mut().expect("Expected at least one node");

    for i in 0..=10 {
        let raw_tx = tx(BASE_CHAIN_ID, None, i, Address::random(), 21_000);
        let signed = TransactionTestContext::sign_tx(signer(0), raw_tx).await;
        test_ctx.rpc.inject_tx(signed.encoded_2718().into()).await?;
    }

    // Create a struct to hold received messages
    let received_messages = Arc::new(Mutex::new(Vec::new()));
    let messages_clone = received_messages.clone();
    let cancellation_token = CancellationToken::new();
    let flashblocks_ws_url = format!("ws://127.0.0.1:{}", 9002);

    // Spawn WebSocket listener task
    let cancellation_token_clone = cancellation_token.clone();
    let ws_handle: JoinHandle<eyre::Result<()>> = tokio::spawn(async move {
        let (ws_stream, _) = connect_async(flashblocks_ws_url).await?;
        let (_, mut read) = ws_stream.split();

        let mut total = 0;
        loop {
            tokio::select! {
              _ = cancellation_token_clone.cancelled() => {
                  break Ok(());
              }
              Some(Ok(Message::Text(text))) = read.next() => {
                info!(target: "payload_builder", "Received message: {text}");

                let payload = serde_json::from_str::<FlashblocksPayloadV1>(&text).expect("Failed to parse FlashblocksPayloadV1");

                let transactions = payload.diff.transactions;
                total += transactions.len();

                info!(target: "payload_builder",  ?total, "Received payload with transactions: {transactions:?}");


                messages_clone.lock().expect("Failed to lock messages").push(text);
              }
            }
        }
    });

    // Initiate a payload building job
    let block = test_ctx.advance_block().await?;

    assert_eq!(block.into_sealed_block().into_body().transactions.len(), 11);

    ws_handle.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_build_flashblocks_valid_pre_state() -> eyre::Result<()> {
    tokio::time::sleep(Duration::from_secs(1)).await;
    reth_tracing::init_test_tracing();
    let (_signers, mut nodes, _task_manager) = setup_swarm(3).await?;

    let test_ctx = nodes.first_mut().expect("Expected at least one node");

    let mut txns: Vec<Bytes> = vec![];

    for i in 0..=11 {
        let raw_tx = tx(BASE_CHAIN_ID, None, 0, Address::random(), 21_000);
        let signed = TransactionTestContext::sign_tx(signer(i as u32), raw_tx).await;
        txns.push(signed.encoded_2718().into());
    }

    // Create a struct to hold received messages
    let received_messages = Arc::new(Mutex::new(Vec::new()));
    let messages_clone = received_messages.clone();
    let cancellation_token = CancellationToken::new();
    let flashblocks_ws_url = format!("ws://127.0.0.1:{}", 9002);

    // Spawn WebSocket listener task
    let cancellation_token_clone = cancellation_token.clone();
    let ws_handle: JoinHandle<eyre::Result<()>> = tokio::spawn(async move {
        let (ws_stream, _) = connect_async(flashblocks_ws_url).await?;
        let (_, mut read) = ws_stream.split();

        let mut total = 0;
        loop {
            tokio::select! {
              _ = cancellation_token_clone.cancelled() => {
                  break Ok(());
              }
              Some(Ok(Message::Text(text))) = read.next() => {
                info!(target: "payload_builder", "Received message: {text}");

                let payload = serde_json::from_str::<FlashblocksPayloadV1>(&text).expect("Failed to parse FlashblocksPayloadV1");

                let transactions = payload.diff.transactions;
                total += transactions.len();

                info!(target: "payload_builder",  ?total, "Received payload with transactions: {transactions:?}");


                messages_clone.lock().expect("Failed to lock messages").push(text);
              }
            }
        }
    });

    // Initiate a payload building job
    let eth_attr = test_ctx.payload.new_payload().await?;

    let mut flashblock_interval = tokio::time::interval(Duration::from_millis(200));
    flashblock_interval.tick().await; // Wait for the first tick

    flashblock_interval.tick().await; // Wait for the next tick
                                      // Inject first bundle of transactions
    for txn in &txns[..4] {
        test_ctx.rpc.inject_tx(txn.clone()).await?;
    }

    flashblock_interval.tick().await; // Wait for the next tick

    for txn in &txns[4..8] {
        test_ctx.rpc.inject_tx(txn.clone()).await?;
    }

    flashblock_interval.tick().await; // Wait for the next tickå

    for txn in &txns[8..12] {
        test_ctx.rpc.inject_tx(txn.clone()).await?;
    }

    // first event is the payload attributes
    test_ctx.payload.expect_attr_event(eth_attr.clone()).await?;

    // wait for the payload builder to have finished building
    test_ctx
        .payload
        .wait_for_built_payload(eth_attr.payload_id())
        .await;

    let payload = test_ctx.payload.expect_built_payload().await?;

    // Cache the built payload
    test_ctx.submit_payload(payload.clone()).await?;

    // trigger forkchoice update via engine api to commit the block to the blockchain
    test_ctx
        .update_forkchoice(payload.block().hash(), payload.block().hash())
        .await?;

    let block = payload.into_sealed_block();
    assert_eq!(block.clone().into_body().transactions.len(), 12);

    // Assert that the aggregate of message `diffs` is equivalent to the transactions body of the sealed block
    let messages = received_messages.lock().expect("Failed to lock messages");
    let mut transactions = HashSet::new();
    for message in messages.iter() {
        if let Ok(payload) = serde_json::from_str::<FlashblocksPayloadV1>(message) {
            transactions.extend(payload.diff.transactions);
        } else {
            warn!(target: "payload_builder", "Failed to parse message as FlashblocksPayloadV1: {message}");
        }
    }

    let block_txs = block.body().transactions_iter();

    for tx in block_txs {
        assert!(
            transactions.contains(&Bytes::from(tx.encoded_2718())),
            "Transaction not found in received messages: {:?}",
            tx
        );
    }

    ws_handle.abort();

    Ok(())
}
