//! Utilities for running world chain builder end-to-end tests.
use alloy_genesis::{Genesis, GenesisAccount};
use alloy_network::eip2718::Encodable2718;
use alloy_network::{Ethereum, EthereumWallet, TransactionBuilder};
use alloy_rpc_types::{TransactionRequest, Withdrawals};
use reth::api::{FullNodeTypesAdapter, NodeTypesWithDBAdapter};
use reth::builder::components::Components;
use reth::builder::engine_tree_config::TreeConfig;
use reth::builder::Node;
use reth::builder::{EngineNodeLauncher, NodeAdapter, NodeBuilder, NodeConfig, NodeHandle};
use reth::payload::{EthPayloadBuilderAttributes, PayloadId};
use reth::tasks::TaskManager;
use reth::transaction_pool::blobstore::DiskFileBlobStore;
use reth::transaction_pool::{Pool, TransactionValidationTaskExecutor};
use reth_db::test_utils::TempDatabase;
use reth_db::DatabaseEnv;
use reth_e2e_test_utils::node::NodeTestContext;
use reth_evm::execute::BasicBlockExecutorProvider;
use reth_node_core::args::RpcServerArgs;
use reth_optimism_chainspec::{OpChainSpec, OpChainSpecBuilder};
use reth_optimism_consensus::OpBeaconConsensus;
use reth_optimism_evm::{OpEvmConfig, OpExecutionStrategyFactory};
use reth_optimism_node::node::OpAddOns;
use reth_optimism_node::{OpNetworkPrimitives, OpPayloadBuilderAttributes};
use reth_optimism_primitives::OpTransactionSigned;
use reth_primitives_traits::SignedTransaction;
use reth_provider::providers::BlockchainProvider;
use revm_primitives::{Address, FixedBytes, B256};
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;
use world_chain_builder_node::args::WorldChainArgs;
use world_chain_builder_node::node::WorldChainNode;
use world_chain_builder_pool::ordering::WorldChainOrdering;
use world_chain_builder_pool::root::LATEST_ROOT_SLOT;
use world_chain_builder_pool::tx::WorldChainPooledTransaction;
use world_chain_builder_pool::validator::WorldChainTransactionValidator;
use world_chain_builder_rpc::{EthApiExtServer, WorldChainEthApiExt};
use world_chain_builder_test_utils::utils::{signer, tree_root};
use world_chain_builder_test_utils::{
    DEV_WORLD_ID, PBH_DEV_ENTRYPOINT, PBH_DEV_SIGNATURE_AGGREGATOR,
};

use world_chain_builder_node::test_utils::{raw_pbh_multicall_bytes, tx};

type NodeTypesAdapter = FullNodeTypesAdapter<
    WorldChainNode,
    Arc<TempDatabase<DatabaseEnv>>,
    BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode, Arc<TempDatabase<DatabaseEnv>>>>,
>;

type NodeHelperType = NodeAdapter<
    NodeTypesAdapter,
    Components<
        NodeTypesAdapter,
        OpNetworkPrimitives,
        Pool<
            TransactionValidationTaskExecutor<
                WorldChainTransactionValidator<
                    BlockchainProvider<
                        NodeTypesWithDBAdapter<WorldChainNode, Arc<TempDatabase<DatabaseEnv>>>,
                    >,
                    WorldChainPooledTransaction,
                >,
            >,
            WorldChainOrdering<WorldChainPooledTransaction>,
            DiskFileBlobStore,
        >,
        OpEvmConfig,
        BasicBlockExecutorProvider<OpExecutionStrategyFactory>,
        Arc<OpBeaconConsensus<OpChainSpec>>,
        world_chain_builder_payload::builder::WorldChainPayloadBuilder<
            BlockchainProvider<
                NodeTypesWithDBAdapter<WorldChainNode, Arc<TempDatabase<DatabaseEnv>>>,
            >,
            DiskFileBlobStore,
        >,
    >,
>;

type Adapter = NodeTestContext<NodeHelperType, OpAddOns<NodeHelperType>>;

pub const BASE_CHAIN_ID: u64 = 8453;

pub struct WorldChainBuilderTestContext {
    pub signers: Range<u32>,
    pub tasks: TaskManager,
    pub node: Adapter,
}

impl WorldChainBuilderTestContext {
    pub async fn setup() -> eyre::Result<Self> {
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
        let builder_args = WorldChainArgs {
            num_pbh_txs: 30,
            verified_blockspace_capacity: 70,
            pbh_entrypoint: PBH_DEV_ENTRYPOINT,
            signature_aggregator: PBH_DEV_SIGNATURE_AGGREGATOR,
            world_id: DEV_WORLD_ID,

            ..Default::default()
        };

        let world_chain_node = WorldChainNode::new(builder_args.clone());
        let builder = NodeBuilder::new(node_config.clone())
            .testing_node(exec.clone())
            .with_types_and_provider::<WorldChainNode, BlockchainProvider<_>>()
            .with_components(world_chain_node.components_builder())
            .with_add_ons(world_chain_node.add_ons())
            .extend_rpc_modules(move |ctx| {
                let provider = ctx.provider().clone();
                let pool = ctx.pool().clone();
                let eth_api_ext = WorldChainEthApiExt::new(pool, provider, None);

                ctx.modules.replace_configured(eth_api_ext.into_rpc())?;
                Ok(())
            });

        let NodeHandle {
            node,
            node_exit_future: _,
        } = builder
            .launch_with_fn(|builder| {
                let launcher = EngineNodeLauncher::new(
                    builder.task_executor().clone(),
                    builder.config().datadir(),
                    TreeConfig::default(),
                );
                builder.launch_with(launcher)
            })
            .await?;

        let test_ctx = NodeTestContext::new(node, optimism_payload_attributes).await?;
        Ok(Self {
            signers: (0..5),
            tasks,
            node: test_ctx,
        })
    }
}

#[tokio::test]
async fn test_can_build_pbh_payload() -> eyre::Result<()> {
    let mut ctx = WorldChainBuilderTestContext::setup().await?;
    let mut pbh_tx_hashes = vec![];
    let signers = ctx.signers.clone();
    for signer in signers.into_iter() {
        let raw_tx = raw_pbh_multicall_bytes(signer, 0, 0, BASE_CHAIN_ID).await;
        let pbh_hash = ctx.node.rpc.inject_tx(raw_tx.clone()).await?;
        pbh_tx_hashes.push(pbh_hash);
    }

    let (payload, _) = ctx.node.advance_block().await?;

    assert_eq!(
        payload.block().body().transactions.len(),
        pbh_tx_hashes.len()
    );
    let block_hash = payload.block().hash();
    let block_number = payload.block().number;

    let tip = pbh_tx_hashes[0];
    ctx.node
        .assert_new_block(tip, block_hash, block_number)
        .await?;

    Ok(())
}

#[tokio::test]
async fn test_transaction_pool_ordering() -> eyre::Result<()> {
    let mut ctx = WorldChainBuilderTestContext::setup().await?;
    let non_pbh_tx = tx(
        ctx.node.inner.chain_spec().chain.id(),
        None,
        0,
        Address::default(),
    );
    let wallet = signer(0);
    let signer = EthereumWallet::from(wallet);
    let signed = <TransactionRequest as TransactionBuilder<Ethereum>>::build(non_pbh_tx, &signer)
        .await
        .unwrap();
    let non_pbh_hash = ctx.node.rpc.inject_tx(signed.encoded_2718().into()).await?;
    let mut pbh_tx_hashes = vec![];
    let signers = ctx.signers.clone();
    for signer in signers.into_iter().skip(1) {
        let raw_tx = raw_pbh_multicall_bytes(signer, 0, 0, BASE_CHAIN_ID).await;
        let pbh_hash = ctx.node.rpc.inject_tx(raw_tx.clone()).await?;
        pbh_tx_hashes.push(pbh_hash);
    }

    let (payload, _) = ctx.node.advance_block().await?;

    assert_eq!(
        payload.block().body().transactions.len(),
        pbh_tx_hashes.len() + 1
    );
    // Assert the non-pbh transaction is included in the block last
    assert_eq!(
        *payload
            .block()
            .body()
            .transactions
            .last()
            .unwrap()
            .tx_hash(),
        non_pbh_hash
    );
    let block_hash = payload.block().hash();
    let block_number = payload.block().number;

    let tip = pbh_tx_hashes[0];
    ctx.node
        .assert_new_block(tip, block_hash, block_number)
        .await?;

    Ok(())
}

#[tokio::test]
async fn test_invalidate_dup_tx_and_nullifier() -> eyre::Result<()> {
    let ctx = WorldChainBuilderTestContext::setup().await?;
    let signer = 0;
    let raw_tx = raw_pbh_multicall_bytes(signer, 0, 0, BASE_CHAIN_ID).await;
    ctx.node.rpc.inject_tx(raw_tx.clone()).await?;
    let dup_pbh_hash_res = ctx.node.rpc.inject_tx(raw_tx.clone()).await;
    assert!(dup_pbh_hash_res.is_err());
    Ok(())
}

#[tokio::test]
async fn test_dup_pbh_nonce() -> eyre::Result<()> {
    let mut ctx = WorldChainBuilderTestContext::setup().await?;
    let signer = 0;

    let raw_tx_0 = raw_pbh_multicall_bytes(signer, 0, 0, BASE_CHAIN_ID).await;
    ctx.node.rpc.inject_tx(raw_tx_0.clone()).await?;
    let raw_tx_1 = raw_pbh_multicall_bytes(signer, 0, 0, BASE_CHAIN_ID).await;

    // Now that the nullifier has successfully been stored in
    // the `ExecutedPbhNullifierTable`, inserting a new tx with the
    // same pbh_nonce should fail to validate.
    assert!(ctx.node.rpc.inject_tx(raw_tx_1.clone()).await.is_err());

    let (payload, _) = ctx.node.advance_block().await?;

    // One transaction should be successfully validated
    // and included in the block.
    assert_eq!(payload.block().body().transactions.len(), 1);

    Ok(())
}

/// Helper function to create a new eth payload attributes
pub fn optimism_payload_attributes(
    timestamp: u64,
) -> OpPayloadBuilderAttributes<OpTransactionSigned> {
    let attributes = EthPayloadBuilderAttributes {
        timestamp,
        prev_randao: B256::ZERO,
        suggested_fee_recipient: Address::ZERO,
        withdrawals: Withdrawals::default(),
        parent_beacon_block_root: Some(B256::ZERO),
        id: PayloadId(FixedBytes::<8>::random()),
        parent: FixedBytes::default(),
    };

    OpPayloadBuilderAttributes {
        payload_attributes: attributes,
        transactions: vec![],
        gas_limit: None,
        no_tx_pool: false,
        eip_1559_params: None,
    }
}

/// Builds an OP Mainnet chain spec with the given merkle root
/// Populated in the OpWorldID contract.
fn get_chain_spec() -> OpChainSpec {
    let mut genesis: Genesis = serde_json::from_str(include_str!("assets/genesis.json")).unwrap();
    genesis.config.chain_id = BASE_CHAIN_ID;

    OpChainSpecBuilder::base_mainnet()
        .genesis(genesis.extend_accounts(vec![(
            DEV_WORLD_ID,
            GenesisAccount::default().with_storage(Some(BTreeMap::from_iter(vec![(
                LATEST_ROOT_SLOT.into(),
                tree_root().into(),
            )]))),
        )]))
        .ecotone_activated()
        .build()
}
