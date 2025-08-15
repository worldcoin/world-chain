//! Utilities for running world chain builder end-to-end tests.
use crate::args::WorldChainArgs;
use crate::flashblocks::WorldChainFlashblocksNode;
use crate::tests::common::EngineDriver;
use crate::tests::get_chain_spec;
use crate::tests::*;

use alloy_consensus::Block;
use alloy_eips::BlockNumberOrTag;
use alloy_rpc_types_engine::PayloadId;
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use futures::StreamExt;
use op_alloy_consensus::OpTxEnvelope;
use reth::api::TreeConfig;
use reth::args::PayloadBuilderArgs;
use reth::builder::Node;
use reth::builder::{EngineNodeLauncher, NodeBuilder, NodeConfig, NodeHandle};
use reth::chainspec::EthChainSpec;
use reth::tasks::TaskManager;
use reth_e2e_test_utils::node::NodeTestContext;
use reth_node_core::args::RpcServerArgs;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::utils::optimism_payload_attributes;
use reth_optimism_node::OpPayloadBuilderAttributes;
use reth_primitives::RecoveredBlock;
use reth_provider::providers::BlockchainProvider;
use reth_provider::BlockReaderIdExt;
use rollup_boost::ed25519_dalek::{SigningKey, VerifyingKey};
use rollup_boost::{Authorization, FlashblocksPayloadV1};
use std::net::{IpAddr, Ipv4Addr};
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tracing::{info, span};
use world_chain_builder_flashblocks::args::FlashblocksArgs;
use world_chain_builder_flashblocks::primitives::{Flashblock, Flashblocks, FlashblocksState};
use world_chain_builder_rpc::{EthApiExtServer, WorldChainEthApiExt};
use world_chain_builder_test_utils::utils::signer;
use world_chain_builder_test_utils::{
    DEV_WORLD_ID, PBH_DEV_ENTRYPOINT, PBH_DEV_SIGNATURE_AGGREGATOR,
};

use reth_node_api::PayloadBuilderAttributes;

pub struct NodeContext {
    pub node: WorldChainNode<WorldChainFlashblocksNode>,
    pub p2p_handle: FlashblocksHandle,
    pub state: FlashblocksState,
    pub builder_vk: VerifyingKey,
    pub _auth_listener: tokio::sync::watch::Receiver<Authorization>,
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
            interval: Duration::from_millis(200),
            ..Default::default()
        })
        .with_unused_ports();

    // discv5 ports seem to be clashing
    node_config.network.discovery.disable_discovery = true;
    node_config.network.discovery.addr = [127, 0, 0, 1].into();

    // is 0.0.0.0 by default
    node_config.network.addr = [127, 0, 0, 1].into();

    let mut node_contexts = Vec::<NodeContext>::with_capacity(num_nodes as usize);

    for idx in 0..num_nodes {
        let span = span!(tracing::Level::INFO, "test_node", idx);
        let _enter = span.enter();
        let flashblocks_args = FlashblocksArgs {
            flashblock_block_time: 1000,
            flashblock_interval: 200,
            flashblock_host: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            flashblock_port: 9002 + idx as u16,
            flashblocks_authorizor_vk: Some(authorizer_vk),
            flashblocks_builder_sk: SigningKey::from_bytes(&[idx; 32]),
            flashblocks_authorization_enabled: true,
        };

        let (flashblocks_tx, _) = broadcast::channel(100);

        let state = FlashblocksState::default();

        let flashblocks_handle = FlashblocksHandle::new(
            authorizer_vk,
            flashblocks_args.flashblocks_builder_sk.clone(),
            flashblocks_tx.clone(),
        );

        let authorization = Authorization::new(
            PayloadId::default(),
            0,
            &flashblocks_args.flashblocks_builder_sk.clone(),
            authorizer_vk,
        );
        let (to_jobs_generator, rx) = tokio::sync::watch::channel(authorization);

        let node = WorldChainFlashblocksNode::new(
            WorldChainArgs {
                verified_blockspace_capacity: 70,
                pbh_entrypoint: PBH_DEV_ENTRYPOINT,
                signature_aggregator: PBH_DEV_SIGNATURE_AGGREGATOR,
                world_id: DEV_WORLD_ID,
                builder_private_key: signer(6).to_bytes().to_string(),
                flashblocks_args: Some(flashblocks_args.clone()),
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

        node.update_forkchoice(op_chain_spec.genesis_hash(), op_chain_spec.genesis_hash())
            .await?;

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
            _auth_listener: rx,
            builder_vk: flashblocks_args
                .flashblocks_builder_sk
                .clone()
                .verifying_key(),
        };

        node_contexts.push(node_ctx);
    }
    Ok((0..6, node_contexts, tasks))
}

#[tokio::test(flavor = "multi_thread")]
async fn test_flashblocks() -> eyre::Result<()> {
    // ------------------------- Simple Flashblock Block -------------------------
    reth_tracing::init_test_tracing();
    let authorizer_sk = SigningKey::from_bytes(&[42; 32]);
    // Create authorizer keys
    let authorizer_vk = authorizer_sk.verifying_key();

    // Setup nodes with flashblocks
    let (_signers, mut nodes, _task_manager) = setup_flashblocks(3, authorizer_vk).await?;

    // Create message collectors for each node to capture p2p flashblocks
    let messages_node_0 = Arc::new(RwLock::new(Vec::<FlashblocksPayloadV1>::new()));
    let messages_node_1 = Arc::new(RwLock::new(Vec::<FlashblocksPayloadV1>::new()));
    let messages_node_2 = Arc::new(RwLock::new(Vec::<FlashblocksPayloadV1>::new()));

    let messages_clone_0 = messages_node_0.clone();
    let messages_clone_1 = messages_node_1.clone();
    let messages_clone_2 = messages_node_2.clone();

    // Get p2p handles for all nodes
    let p2p_handle_0 = nodes[0].p2p_handle.clone();
    let p2p_handle_1 = nodes[1].p2p_handle.clone();
    let p2p_handle_2 = nodes[2].p2p_handle.clone();

    let state_0 = nodes[0].state.clone();
    let state_1 = nodes[1].state.clone();
    let state_2 = nodes[2].state.clone();

    // Spawn tasks to listen for flashblocks from all nodes
    let _handle = tokio::spawn(async move {
        let fut_0 = async move {
            let mut stream = p2p_handle_0.flashblock_stream();
            while let Some(message) = stream.next().await {
                info!(
                    target: "reth::e2e::flashblocks::auth",
                    "Node 0 received flashblock payload: {} with {} txs",
                    message.payload_id,
                    message.diff.transactions.len()
                );
                messages_clone_0.write().await.push(message);
            }
        };

        let fut_1 = async move {
            let mut stream = p2p_handle_1.flashblock_stream();
            while let Some(message) = stream.next().await {
                info!(
                    target: "reth::e2e::flashblocks::auth",
                    "Node 1 received flashblock payload: {} with {} txs",
                    message.payload_id,
                    message.diff.transactions.len()
                );
                messages_clone_1.write().await.push(message);
            }
        };

        let fut_2 = async move {
            let mut stream = p2p_handle_2.flashblock_stream();
            while let Some(message) = stream.next().await {
                info!(
                    target: "reth::e2e::flashblocks::auth",
                    "Node 2 received flashblock payload: {} with {} txs",
                    message.payload_id,
                    message.diff.transactions.len()
                );
                messages_clone_2.write().await.push(message);
            }
        };

        tokio::join!(fut_0, fut_1, fut_2);
    });

    let builder_vk = nodes[0].builder_vk;

    let authorization = |attributes: OpPayloadBuilderAttributes<OpTxEnvelope>| {
        Authorization::new(
            attributes.payload_id(),
            attributes.timestamp(),
            &authorizer_sk,
            builder_vk,
        )
    };

    let mut driver = EngineDriver::default();

    let current_head = nodes[0]
        .node
        .inner
        .provider()
        .sealed_header_by_number_or_tag(BlockNumberOrTag::Latest)
        .unwrap()
        .unwrap();

    let built_payload = driver
        .drive(10, authorization, &mut nodes[0].node, current_head.hash())
        .await?
        .unwrap();

    let aggr_state_1 = Flashblock::reduce(Flashblocks(state_1.0.read().await.0.clone()));
    let aggr_state_2 = Flashblock::reduce(Flashblocks(state_2.0.read().await.0.clone()));

    println!(
        "Node 0 has {} flashblocks in state",
        state_0.0.read().await.0.len()
    );
    println!(
        "Node 1 has {} flashblocks in state",
        state_1.0.read().await.0.len()
    );
    println!(
        "Node 2 has {} flashblocks in state",
        state_2.0.read().await.0.len()
    );

    let block_1 = RecoveredBlock::<Block<OpTxEnvelope>>::try_from(aggr_state_1.clone().unwrap())
        .expect("Failed to recover block from flashblocks state on node 1");
    let block_2 = RecoveredBlock::<Block<OpTxEnvelope>>::try_from(aggr_state_2.clone().unwrap())
        .expect("Failed to recover block from flashblocks state on node 2");

    assert_eq!(
        built_payload
            .clone()
            .into_sealed_block()
            .header()
            .hash_slow(),
        block_1.sealed_block().header().hash_slow(),
        "Node 1 should have the same block as the built payload"
    );

    assert_eq!(
        built_payload
            .clone()
            .into_sealed_block()
            .header()
            .hash_slow(),
        block_2.sealed_block().header().hash_slow(),
        "Node 2 should have the same block as the built payload"
    );

    let hash = built_payload.clone().into_sealed_block().hash();

    // Submit the built payload to all nodes
    nodes[0].node.submit_payload(built_payload.clone()).await?;
    nodes[1].node.submit_payload(built_payload.clone()).await?;
    nodes[2].node.submit_payload(built_payload.clone()).await?;

    driver.gen = |_| None;

    // Make the latest flashblock the cannonical head of
    let _ = driver
        .drive(0, authorization, &mut nodes[0].node, hash)
        .await?;
    let _ = driver
        .drive(0, authorization, &mut nodes[1].node, hash)
        .await?;
    let _ = driver
        .drive(0, authorization, &mut nodes[2].node, hash)
        .await?;

    // state should be cleared after FCU
    assert!(state_0.0.read().await.0.is_empty());
    assert!(state_1.0.read().await.0.is_empty());
    assert!(state_2.0.read().await.0.is_empty());

    // ------------------------ Sequencer Failover ----------------------------------

    Ok(())
}
