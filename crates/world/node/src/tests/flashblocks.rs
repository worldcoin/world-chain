//! Utilities for running world chain builder end-to-end tests.
use crate::args::WorldChainArgs;
use crate::flashblocks::WorldChainFlashblocksNode;
use crate::node::WorldChainNode as OtherWorldChainNode;
use alloy_genesis::{Genesis, GenesisAccount};
use alloy_network::eip2718::Encodable2718;
use alloy_network::{Ethereum, EthereumWallet, TransactionBuilder};
use alloy_rpc_types::{TransactionRequest, Withdrawals};
use flashblocks::args::FlashblockArgs;
use op_alloy_consensus::OpTxEnvelope;
use reth::api::{NodeTypesWithDBAdapter, TreeConfig};
use reth::builder::Node;
use reth::builder::{EngineNodeLauncher, NodeBuilder, NodeConfig, NodeHandle};
use reth::payload::{EthPayloadBuilderAttributes, PayloadId};
use reth::tasks::TaskManager;
use reth_e2e_test_utils::node::NodeTestContext;
use reth_e2e_test_utils::transaction::TransactionTestContext;
use reth_e2e_test_utils::{NodeBuilderHelper, NodeHelperType, TmpDB};
use reth_engine_local::LocalPayloadAttributesBuilder;
use reth_node_api::{
    FullNodeComponents, FullNodeTypes, NodeTypes, PayloadAttributesBuilder, PayloadTypes, TxTy,
};
use reth_node_builder::components::NetworkBuilder;
use reth_node_builder::rpc::RethRpcAddOns;
use reth_node_builder::{NodeComponents, NodeComponentsBuilder};
use reth_node_core::args::RpcServerArgs;
use reth_optimism_chainspec::{OpChainSpec, OpChainSpecBuilder};
use reth_optimism_node::utils::optimism_payload_attributes;
use reth_optimism_node::OpPayloadBuilderAttributes;
use reth_optimism_primitives::OpTransactionSigned;
use reth_primitives::Transaction;
use reth_provider::providers::{BlockchainProvider, NodeTypesForProvider};
use reth_transaction_pool::{Pool, TransactionPool};
use revm_primitives::{Address, FixedBytes, B256, U256};
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;
use world_chain_builder_pool::root::LATEST_ROOT_SLOT;
use world_chain_builder_pool::tx::{WorldChainPoolTransaction, WorldChainPooledTransaction};
use world_chain_builder_pool::validator::{MAX_U16, PBH_GAS_LIMIT_SLOT, PBH_NONCE_LIMIT_SLOT};
use world_chain_builder_pool::WorldChainTransactionPool;
use world_chain_builder_rpc::{EthApiExtServer, WorldChainEthApiExt};
use world_chain_builder_test_utils::utils::{signer, tree_root};
use world_chain_builder_test_utils::{
    DEV_WORLD_ID, PBH_DEV_ENTRYPOINT, PBH_DEV_SIGNATURE_AGGREGATOR,
};

use crate::test_utils::{raw_pbh_bundle_bytes, tx};

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
        flashblock_args: Some(FlashblockArgs {
            flashblock_port: 8080,
            flashblock_block_time: 2000,
            flashblock_interval: 200,
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

    let test_ctx = NodeTestContext::new(node, optimism_payload_attributes::<OpTxEnvelope>).await?;

    Ok((0..6, test_ctx, tasks))
}

#[tokio::test(flavor = "multi_thread")]
async fn test_flashblocks() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();
    let (signers, mut test_ctx, task_manager) = setup_flashblocks().await?;

    for i in 0..=10 {
        let raw_tx = tx(BASE_CHAIN_ID, None, i, Address::random());
        let signed = TransactionTestContext::sign_tx(signer(0), raw_tx).await;
        test_ctx.rpc.inject_tx(signed.encoded_2718().into()).await?;
    }

    let payload = test_ctx.advance_block().await?;

    // One transaction should be successfully validated
    // and included in the block.
    assert_eq!(payload.block().body().transactions.len(), 2);

    Ok(())
}
