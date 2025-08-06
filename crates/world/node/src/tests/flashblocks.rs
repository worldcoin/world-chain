//! Utilities for running world chain builder end-to-end tests.
use crate::args::WorldChainArgs;
use crate::flashblocks::rpc::WorldChainEngineApiBuilder;
use crate::flashblocks::WorldChainFlashblocksNode;
use alloy_eips::Decodable2718;
use alloy_network::eip2718::Encodable2718;
use alloy_primitives::{Bytes, TxHash};
use flashblocks::args::FlashblocksArgs;
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use futures_util::StreamExt;
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
use reth_optimism_node::{OpAddOns, OpEngineValidatorBuilder};
use reth_provider::providers::BlockchainProvider;
use revm_primitives::Address;
use rollup_boost::ed25519_dalek::SigningKey;
use rollup_boost::FlashblocksPayloadV1;
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr};
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{span, warn};
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
    Vec<FlashblocksHandle>,
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

    let mut nodes =
        Vec::<WorldChainNode<WorldChainFlashblocksNode>>::with_capacity(num_nodes as usize);

    let mut flashblocks_handles = Vec::with_capacity(num_nodes as usize);

    for idx in 0..num_nodes {
        let span = span!(tracing::Level::INFO, "test_node", idx);
        let _enter = span.enter();
        let flashblocks_args = FlashblocksArgs {
            flashblock_block_time: 1000,
            flashblock_interval: 200,
            flashblock_host: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            flashblock_port: 9002 + idx as u16,
            flashblocks_authorizor_vk: None,
            flashblocks_builder_sk: SigningKey::from_bytes(&[idx; 32]),
        };
        let (flashblocks_tx, _) = tokio::sync::broadcast::channel(100);

        let authorizer_vk = SigningKey::from_bytes(&[10; 32]);
        let flashblocks_handle = FlashblocksHandle::new(
            authorizer_vk.verifying_key(),
            flashblocks_args.flashblocks_builder_sk.clone(),
            flashblocks_tx,
        );

        let engine_builder = WorldChainEngineApiBuilder {
            engine_validator_builder: OpEngineValidatorBuilder::default(),
            flashblocks_handle: flashblocks_handle.clone(),
        };

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
            flashblocks_handle.clone(),
        );

        let NodeHandle {
            node,
            node_exit_future: _,
        } = NodeBuilder::new(node_config.clone())
            .testing_node(exec.clone())
            .with_types_and_provider::<WorldChainFlashblocksNode, BlockchainProvider<_>>()
            .with_components(node.components_builder())
            .with_add_ons(OpAddOns::default().with_engine_api(engine_builder))
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
        flashblocks_handles.push(flashblocks_handle);
    }
    Ok((0..6, nodes, tasks, flashblocks_handles))
}

#[tokio::test(flavor = "multi_thread")]
async fn test_flashblocks() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();
    // TODO:
    let (_signers, mut nodes, _task_manager, p2p_handles) = setup_flashblocks(3).await?;

    // fetch the first node
    let node: &mut WorldChainNode<WorldChainFlashblocksNode> = nodes
        .first_mut()
        .expect("At least one node should be present");

    let messages = Arc::new(RwLock::new(Vec::<FlashblocksPayloadV1>::new()));

    let messages_clone: Arc<RwLock<Vec<FlashblocksPayloadV1>>> = messages.clone();
    let _handle = tokio::spawn(async move {
        let flashblocks_handle = p2p_handles
            .first()
            .expect("At least one p2p handle should be present")
            .clone();

        let mut stream = flashblocks_handle.flashblock_stream();
        while let Some(message) = stream.next().await {
            messages_clone.write().await.push(message);
        }
    });
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
