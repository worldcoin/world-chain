//! Utilities for running world chain builder end-to-end tests.
use crate::args::WorldChainArgs;
use crate::flashblocks::WorldChainFlashblocksNode;
use alloy_eips::Decodable2718;
use alloy_network::eip2718::Encodable2718;
use alloy_primitives::{Bytes, TxHash};
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
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
use rollup_boost::ed25519_dalek::{SigningKey, VerifyingKey};
use rollup_boost::{Authorization, FlashblocksPayloadV1};
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr};
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tracing::{info, span};
use world_chain_builder_flashblocks::args::FlashblocksArgs;
use world_chain_builder_flashblocks::primitives::FlashblocksState;
use world_chain_builder_rpc::{EthApiExtServer, WorldChainEthApiExt};
use world_chain_builder_test_utils::utils::signer;
use world_chain_builder_test_utils::{
    DEV_WORLD_ID, PBH_DEV_ENTRYPOINT, PBH_DEV_SIGNATURE_AGGREGATOR,
};

use crate::test_utils::tx;
use crate::tests::{get_chain_spec, WorldChainBuilderTestContext, WorldChainNode, BASE_CHAIN_ID};

use reth_node_api::PayloadBuilderAttributes;

pub struct NodeContext {
    pub node: WorldChainNode<WorldChainFlashblocksNode>,
    pub p2p_handle: FlashblocksHandle,
    pub state: FlashblocksState,
    pub auth_listener: tokio::sync::watch::Receiver<Option<Authorization>>,
}

pub async fn setup_flashblocks(
    num_nodes: u8,
    authorizer_vk: VerifyingKey,
) -> eyre::Result<(Range<u8>, Vec<NodeContext>, TaskManager)> {
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
            gas_limit: Some(25_000_000),
            interval: Duration::from_millis(1000),
            ..Default::default()
        })
        .with_unused_ports();

    // discv5 ports seem to be clashing
    node_config.network.discovery.disable_discovery = true;
    node_config.network.discovery.addr = [127, 0, 0, 1].into();

    // is 0.0.0.0 by default
    node_config.network.addr = [127, 0, 0, 1].into();

    let mut node_contexts = Vec::<NodeContext>::with_capacity(num_nodes as usize);
    let auth_sk = SigningKey::from_bytes(&[10; 32]);
    for idx in 0..num_nodes {
        let span = span!(tracing::Level::INFO, "test_node", idx);
        let _enter = span.enter();
        let flashblocks_args = FlashblocksArgs {
            flashblock_block_time: 1000,
            flashblock_interval: 200,
            flashblock_host: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            flashblock_port: 9002 + idx as u16,
            flashblocks_authorizor_vk: Some(auth_sk.verifying_key()),
            flashblocks_builder_sk: SigningKey::from_bytes(&[idx; 32]),
            flashblocks_authorizer_sk: Some(auth_sk.clone()),
            flashblocks_authorization_enabled: true,
        };

        let (flashblocks_tx, _) = broadcast::channel(100);

        let state = FlashblocksState::default();

        let flashblocks_handle = FlashblocksHandle::new(
            authorizer_vk,
            flashblocks_args.flashblocks_builder_sk.clone(),
            flashblocks_tx.clone(),
        );

        let (to_jobs_generator, _) = tokio::sync::watch::channel(None::<Authorization>);

        let node = WorldChainFlashblocksNode::new(
            WorldChainArgs {
                verified_blockspace_capacity: 70,
                pbh_entrypoint: PBH_DEV_ENTRYPOINT,
                signature_aggregator: PBH_DEV_SIGNATURE_AGGREGATOR,
                world_id: DEV_WORLD_ID,
                builder_private_key: signer(6).to_bytes().to_string(),
                flashblocks_args: Some(flashblocks_args),
                ..Default::default()
            },
            state.clone(),
            Some(flashblocks_handle.clone()),
            to_jobs_generator.clone(),
        );

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
        if let Some(previous_node) = node_contexts.last_mut() {
            previous_node.node.connect(&mut node).await;
        }
        // Connect last node with the first if there are more than two
        if idx + 1 == num_nodes && num_nodes > 2 {
            if let Some(first_node) = node_contexts.first_mut() {
                node.connect(&mut first_node.node).await;
            }
        }
        let node_ctx = NodeContext {
            node,
            p2p_handle: flashblocks_handle,
            state,
            auth_listener: to_jobs_generator.subscribe(),
        };

        node_contexts.push(node_ctx);
    }
    Ok((0..6, node_contexts, tasks))
}

#[tokio::test(flavor = "multi_thread")]
async fn test_flashblocks() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();
    let authorizer_vk = SigningKey::from_bytes(&[1; 32]).verifying_key();
    let (signers_, mut nodes, _task_manager) = setup_flashblocks(3, authorizer_vk).await?;

    let node = nodes
        .first_mut()
        .expect("At least one node should be present");

    let messages = Arc::new(RwLock::new(Vec::<FlashblocksPayloadV1>::new()));

    let messages_clone: Arc<RwLock<Vec<FlashblocksPayloadV1>>> = messages.clone();
    let p2p_handle = node.p2p_handle.clone();
    let _handle = tokio::spawn(async move {
        let mut stream = p2p_handle.flashblock_stream();
        while let Some(message) = stream.next().await {
            info!(
                target: "reth::e2e::flashblocks",
                "Received flashblock payload: {} with {} txs",
                message.payload_id,
                message.diff.transactions.len()
            );
            messages_clone.write().await.push(message);
        }
    });

    let node = &mut node.node;
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

    let block = payload.into_sealed_block();

    info!(
        target: "reth::e2e::flashblocks",
        "Built block #{} with {} txs",
        block.header().number,
        block.body().transactions.len()
    );

    let payload_transactions = block
        .body()
        .transactions()
        .map(|tx| tx.hash())
        .collect::<HashSet<_>>();

    let mut flashblock_payload_txns: HashSet<TxHash> = HashSet::new();

    let flashblock_payloads = messages.read().await;
    println!(
        "Received {} flashblocks payloads",
        flashblock_payloads.len()
    );
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

    // // Assert that all transactions in the block are present in the flashblocks payloads
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
        payload.into_sealed_block().header(),
        block.header(),
        "World Chain Node should have built the same block as the Flashblocks Node"
    );

    Ok(())
}

// #[tokio::test(flavor = "multi_thread")]
// async fn test_flashblocks_failover() -> eyre::Result<()> {
//     reth_tracing::init_test_tracing();
//     let authorizer_vk = SigningKey::from_bytes(&[0; 32]).verifying_key();
//     let (_signers, mut nodes, _task_manager, p2p_handles) =
//         setup_flashblocks(33, authorizer_vk).await?;

//     let messages_node_0 = Arc::new(RwLock::new(Vec::<FlashblocksPayloadV1>::new()));
//     let messages_node_1 = Arc::new(RwLock::new(Vec::<FlashblocksPayloadV1>::new()));

//     let messages_clone_node_0: Arc<RwLock<Vec<FlashblocksPayloadV1>>> = messages_node_0.clone();
//     let messages_clone_node_1: Arc<RwLock<Vec<FlashblocksPayloadV1>>> = messages_node_1.clone();

//     let _handle = tokio::spawn(async move {
//         let flashblocks_handle_0 = p2p_handles
//             .first()
//             .expect("At least one p2p handle should be present")
//             .clone();

//         let flashblocks_handle_1 = p2p_handles
//             .get(1)
//             .expect("At least two p2p handles should be present")
//             .clone();

//         let fut_0 = async move {
//             let mut stream = flashblocks_handle_0.flashblock_stream();
//             while let Some(message) = stream.next().await {
//                 messages_clone_node_0.write().await.push(message);
//             }
//         };

//         let fut_1 = async move {
//             let mut stream = flashblocks_handle_1.flashblock_stream();
//             while let Some(message) = stream.next().await {
//                 messages_clone_node_1.write().await.push(message);
//             }
//         };

//         tokio::join!(fut_0, fut_1);
//     });

//     // insert 10 transactions into the pool of the first node
//     for i in 0..10 {
//         let raw_tx = tx(BASE_CHAIN_ID, None, i, Address::random(), 21_000);
//         let signed = TransactionTestContext::sign_tx(signer(0), raw_tx).await; // use a new signer for each transaction
//         nodes[0].rpc.inject_tx(signed.encoded_2718().into()).await?;
//     }

//     // insert 10 transactions into the pool of the second node
//     for i in 0..10 {
//         let raw_tx = tx(BASE_CHAIN_ID, None, i + 10, Address::random(), 21_000);
//         let signed = TransactionTestContext::sign_tx(signer(1), raw_tx).await; // use a new signer for each transaction
//         nodes[1].rpc.inject_tx(signed.encoded_2718().into()).await?;
//     }

//     let mut interval = tokio::time::interval(Duration::from_millis(400));
//     // trigger new payload building job draining the pool of node_0
//     let eth_attr = nodes[0].payload.new_payload().await.unwrap();

//     // first event is the payload attributes
//     nodes[0].payload.expect_attr_event(eth_attr.clone()).await?;

//     interval.tick().await;
//     interval.tick().await;

//     // trigger a payload building job on node_1
//     let eth_attr_1 = nodes[1].payload.new_payload().await.unwrap();

//     // first event is the payload attributes
//     nodes[1]
//         .payload
//         .expect_attr_event(eth_attr_1.clone())
//         .await?;

//     // wait for the payload builder to have finished building on node_0
//     nodes[0]
//         .payload
//         .wait_for_built_payload(eth_attr.payload_id())
//         .await;

//     // wait for the payload builder to have finished building on node_1
//     nodes[1]
//         .payload
//         .wait_for_built_payload(eth_attr_1.payload_id())
//         .await;

//     // ensure we're also receiving the built payload as event
//     let payload = nodes[0].payload.expect_built_payload().await?;
//     let payload_1 = nodes[1].payload.expect_built_payload().await?;

//     assert_eq!(
//         payload.clone().into_sealed_block().header(),
//         payload_1.clone().into_sealed_block().header(),
//         "Both nodes should have built the same block"
//     );

//     let flashblock_payloads_0 = messages_node_0.read().await;
//     let flashblock_payloads_1 = messages_node_1.read().await;

//     let payloads_node_0 = flashblock_payloads_0
//         .iter()
//         .map(|fb| fb.payload_id)
//         .collect::<HashSet<_>>();

//     let payloads_node_1 = flashblock_payloads_1
//         .iter()
//         .map(|fb| fb.payload_id)
//         .collect::<HashSet<_>>();

//     assert_eq!(
//         payloads_node_0, payloads_node_1,
//         "Both nodes should have received the same flashblocks payloads"
//     );

//     Ok(())
// }
