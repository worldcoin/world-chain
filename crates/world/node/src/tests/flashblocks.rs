//! Utilities for running world chain builder end-to-end tests.
use crate::args::WorldChainArgs;
use crate::flashblocks::WorldChainFlashblocksNode;
use alloy_eips::Decodable2718;
use alloy_network::eip2718::Encodable2718;
use alloy_primitives::{Bytes, TxHash};
use flashblocks::args::FlashblocksArgs;
use futures::StreamExt;
use op_alloy_consensus::OpTxEnvelope;
use reth::api::TreeConfig;
use reth::args::PayloadBuilderArgs;
use reth::builder::Node;
use reth::builder::{EngineNodeLauncher, NodeBuilder, NodeConfig, NodeHandle};
use reth::tasks::TaskManager;
use reth_e2e_test_utils::node::NodeTestContext;
use reth_e2e_test_utils::transaction::TransactionTestContext;
use reth_node_core::args::RpcServerArgs;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::utils::optimism_payload_attributes;
use reth_provider::providers::BlockchainProvider;
use revm_primitives::Address;
use rollup_boost::ed25519_dalek::SigningKey;
use rollup_boost::FlashblocksPayloadV1;
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr};
use std::ops::Range;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::span;
use world_chain_builder_rpc::{EthApiExtServer, WorldChainEthApiExt};
use world_chain_builder_test_utils::utils::signer;
use world_chain_builder_test_utils::{
    DEV_WORLD_ID, PBH_DEV_ENTRYPOINT, PBH_DEV_SIGNATURE_AGGREGATOR,
};

use crate::test_utils::tx;
use crate::tests::{get_chain_spec, WorldChainBuilderTestContext, WorldChainNode, BASE_CHAIN_ID};

use reth_node_api::PayloadBuilderAttributes;

pub async fn setup_flashblocks(
    num_nodes: u8,
) -> eyre::Result<(
    Range<u8>,
    Vec<WorldChainNode<WorldChainFlashblocksNode>>,
    TaskManager,
)> {
    std::env::set_var("PRIVATE_KEY", DEV_WORLD_ID.to_string());
    let op_chain_spec = Arc::new(get_chain_spec());

    let tasks = TaskManager::current();
    let exec = tasks.executor();

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
            interval: Duration::from_millis(2000),
            ..Default::default()
        })
        .with_unused_ports();

    // discv5 ports seem to be clashing
    node_config.network.discovery.disable_discovery = true;
    node_config.network.discovery.addr = [127, 0, 0, 1].into();

    // is 0.0.0.0 by default
    node_config.network.addr = [127, 0, 0, 1].into();

    let mut nodes =
        Vec::<WorldChainNode<WorldChainFlashblocksNode>>::with_capacity(num_nodes as usize);

    for idx in 0..num_nodes {
        let span = span!(tracing::Level::INFO, "test_node", idx);
        let _enter = span.enter();

        let flashblocks_args = FlashblocksArgs {
            flashblock_block_time: 2000,
            flashblock_interval: 200,
            flashblock_host: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            flashblock_port: 9002 + idx as u16,
            flashblocks_authorizor_vk: None,
            flashblocks_builder_sk: SigningKey::from_bytes(&[0; 32]),
        };

        let node = WorldChainFlashblocksNode::new(WorldChainArgs {
            verified_blockspace_capacity: 70,
            pbh_entrypoint: PBH_DEV_ENTRYPOINT,
            signature_aggregator: PBH_DEV_SIGNATURE_AGGREGATOR,
            world_id: DEV_WORLD_ID,
            builder_private_key: signer(6).to_bytes().to_string(),
            flashblocks_args: Some(flashblocks_args),
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
    Ok((0..6, nodes, tasks))
}

#[tokio::test(flavor = "multi_thread")]
async fn test_flashblocks() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();
    let (_signers, mut nodes, _task_manager) = setup_flashblocks(3).await?;

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
        loop {
            tokio::select! {
              _ = cancellation_token_clone.cancelled() => {
                  break Ok(());
              }
                Some(Ok(Message::Text(text))) = read.next() => {
                    let payload = serde_json::from_str::<FlashblocksPayloadV1>(&text).expect("Failed to parse FlashblocksPayloadV1");

                    messages_clone.lock().expect("Failed to lock messages").push(payload);
                }
            }
        }
    });

    // fetch the first node
    let node: &mut WorldChainNode<WorldChainFlashblocksNode> = nodes
        .first_mut()
        .expect("At least one node should be present");

    let mut txns: Vec<Bytes> = vec![];

    for i in 0..10 {
        let raw_tx = tx(BASE_CHAIN_ID, None, 0, Address::random(), 21_000);
        let signed = TransactionTestContext::sign_tx(signer(i as u32), raw_tx).await; // use a new signer for each transaction
        txns.push(signed.encoded_2718().into());
    }

    let mut flashblocks_interval = tokio::time::interval(Duration::from_millis(200));
    flashblocks_interval.tick().await;

    // trigger new payload building draining the pool
    let eth_attr = node.payload.new_payload().await.unwrap();

    // first event is the payload attributes
    node.payload.expect_attr_event(eth_attr.clone()).await?;

    for tx in &txns[..4] {
        node.rpc.inject_tx(tx.clone()).await?;
    }

    // insert transactions into the pool after the first flashblock.
    flashblocks_interval.tick().await;
    for tx in &txns[4..] {
        node.rpc.inject_tx(tx.clone()).await?;
    }

    // wait for the payload builder to have finished building
    node.payload
        .wait_for_built_payload(eth_attr.payload_id())
        .await;

    // ensure we're also receiving the built payload as event
    let payload = node.payload.expect_built_payload().await?;

    let flashblock_payloads = received_messages.lock().expect("Failed to lock messages");
    let block = payload.into_sealed_block();

    let payload_transactions = block
        .body()
        .transactions()
        .map(|tx| tx.hash())
        .collect::<HashSet<_>>();

    let mut flashblock_payload_txns: HashSet<TxHash> = HashSet::new();

    for fb_payload in flashblock_payloads.iter() {
        assert_eq!(fb_payload.payload_id, eth_attr.payload_id());
        flashblock_payload_txns.extend(fb_payload.diff.transactions.iter().map(|tx| {
            let decoded =
                OpTxEnvelope::decode_2718(&mut tx.as_ref()).expect("Failed to decode transaction");
            decoded.hash().clone()
        }));
    }

    // Assert that block transactions length is the aggregate of all the flashblock payload diffs
    assert_eq!(
        flashblock_payload_txns.len(),
        payload_transactions.len(),
        "Flashblocks payloads should contain all transactions from the built payload"
    );

    // Assert that all transactions in the block are present in the flashblocks payloads
    for tx in payload_transactions {
        assert!(
            flashblock_payload_txns.contains(tx),
            "Flashblocks payloads should contain transaction: {:?}",
            tx
        );
    }

    // Assert all transactions have been accounted for
    assert_eq!(
        block.body().transactions.len(),
        10,
        "Block should contain all transactions"
    );

    // Spin up World Chain Node with Cannonical Payload Builder
    let mut world_chain_node_test_context = WorldChainBuilderTestContext::setup().await?;

    // Inject all of the same transactions
    for tx in &block.body().transactions {
        world_chain_node_test_context
            .node
            .rpc
            .inject_tx(tx.encoded_2718().into())
            .await?;
    }

    // Build the block
    let payload = world_chain_node_test_context.node.advance_block().await?;

    // Assert the blocks match
    assert_eq!(
        payload.into_sealed_block().header().state_root, // Our state roots mismatch FML
        block.header().state_root,
        "World Chain Node should have built the same block as the Flashblocks Node"
    );

    ws_handle.abort();

    Ok(())
}
