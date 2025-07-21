//! Utilities for running world chain builder end-to-end tests.
use crate::args::WorldChainArgs;
use crate::flashblocks::WorldChainFlashblocksNode;
use alloy_network::eip2718::Encodable2718;
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
use reth_node_core::args::RpcServerArgs;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::utils::optimism_payload_attributes;
use reth_provider::providers::BlockchainProvider;
use revm_primitives::Address;
use rollup_boost::FlashblocksPayloadV1;
use std::ops::Range;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::info;
use world_chain_builder_rpc::{EthApiExtServer, WorldChainEthApiExt};
use world_chain_builder_test_utils::utils::signer;
use world_chain_builder_test_utils::{
    DEV_WORLD_ID, PBH_DEV_ENTRYPOINT, PBH_DEV_SIGNATURE_AGGREGATOR,
};

use crate::test_utils::tx;

use crate::tests::{get_chain_spec, WorldChainNode, BASE_CHAIN_ID};

pub async fn setup_flashblocks() -> eyre::Result<(
    Range<u8>,
    WorldChainNode<WorldChainFlashblocksNode>,
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
            deadline: Duration::from_millis(1500),
            max_payload_tasks: 1,
            gas_limit: Some(35_000_000),
            ..Default::default()
        })
        .with_unused_ports();

    // discv5 ports seem to be clashing
    node_config.network.discovery.disable_discovery = true;
    node_config.network.discovery.addr = [127, 0, 0, 1].into();

    // is 0.0.0.0 by default
    node_config.network.addr = [127, 0, 0, 1].into();

    let node = WorldChainFlashblocksNode::new(WorldChainArgs {
        verified_blockspace_capacity: 70,
        pbh_entrypoint: PBH_DEV_ENTRYPOINT,
        signature_aggregator: PBH_DEV_SIGNATURE_AGGREGATOR,
        world_id: DEV_WORLD_ID,
        builder_private_key: signer(6).to_bytes().to_string(),
        flashblock_args: Some(FlashblockArgs::default()),
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

    let test_ctx = NodeTestContext::new(node, optimism_payload_attributes::<OpTxEnvelope>).await?;

    Ok((0..6, test_ctx, tasks))
}

#[tokio::test(flavor = "multi_thread")]
async fn test_build_flashblocks() -> eyre::Result<()> {
    tokio::time::sleep(Duration::from_secs(1)).await; // Allow time for the node to start
    reth_tracing::init_test_tracing();
    let (_signers, mut test_ctx, _task_manager) = setup_flashblocks().await?;

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
