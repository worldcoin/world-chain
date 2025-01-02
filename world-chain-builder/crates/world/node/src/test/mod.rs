//! Utilities for running world chain builder end-to-end tests.
use alloy_genesis::{Genesis, GenesisAccount};
use alloy_network::eip2718::Encodable2718;
use alloy_network::{Ethereum, EthereumWallet, TransactionBuilder};
use alloy_rpc_types::{TransactionRequest, Withdrawals};
use reth::api::{FullNodeTypesAdapter, NodeTypesWithDBAdapter};
use reth::builder::components::Components;
use reth::builder::{NodeAdapter, NodeBuilder, NodeConfig, NodeHandle};
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
use reth_optimism_node::OpPayloadBuilderAttributes;
use reth_provider::providers::BlockchainProvider;
use revm_primitives::{Address, Bytes, FixedBytes, B256, U256};
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

use world_chain_builder_pool::ordering::WorldChainOrdering;
use world_chain_builder_pool::root::{LATEST_ROOT_SLOT, OP_WORLD_ID};
use world_chain_builder_pool::test_utils::{
    signer, tree_root, PBH_TEST_SIGNATURE_AGGREGATOR, PBH_TEST_VALIDATOR,
};
use world_chain_builder_pool::tx::WorldChainPooledTransaction;
use world_chain_builder_pool::validator::WorldChainTransactionValidator;
use world_chain_builder_rpc::{EthApiExtServer, WorldChainEthApiExt};

use crate::args::{ExtArgs, WorldChainBuilderArgs};
use crate::node::WorldChainBuilder;
use crate::test_utils::{tx, PBHTransactionTestContext, DEV_CHAIN_ID};

type NodeAdapterType = NodeAdapter<
    FullNodeTypesAdapter<
        NodeTypesWithDBAdapter<WorldChainBuilder, Arc<TempDatabase<DatabaseEnv>>>,
        BlockchainProvider<
            NodeTypesWithDBAdapter<WorldChainBuilder, Arc<TempDatabase<DatabaseEnv>>>,
        >,
    >,
    Components<
        FullNodeTypesAdapter<
            NodeTypesWithDBAdapter<WorldChainBuilder, Arc<TempDatabase<DatabaseEnv>>>,
            BlockchainProvider<
                NodeTypesWithDBAdapter<WorldChainBuilder, Arc<TempDatabase<DatabaseEnv>>>,
            >,
        >,
        Pool<
            TransactionValidationTaskExecutor<
                WorldChainTransactionValidator<
                    BlockchainProvider<
                        NodeTypesWithDBAdapter<WorldChainBuilder, Arc<TempDatabase<DatabaseEnv>>>,
                    >,
                    WorldChainPooledTransaction,
                >,
            >,
            WorldChainOrdering<WorldChainPooledTransaction>,
            DiskFileBlobStore,
        >,
        OpEvmConfig,
        BasicBlockExecutorProvider<OpExecutionStrategyFactory>,
        Arc<OpBeaconConsensus>,
    >,
>;

type Adapter = NodeTestContext<NodeAdapterType, OpAddOns<NodeAdapterType>>;

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

        let builder = NodeBuilder::new(node_config.clone())
            .testing_node(exec.clone())
            .node(WorldChainBuilder::new(ExtArgs {
                builder_args: WorldChainBuilderArgs {
                    num_pbh_txs: 30,
                    verified_blockspace_capacity: 70,
                    pbh_validator: PBH_TEST_VALIDATOR,
                    signature_aggregator: PBH_TEST_SIGNATURE_AGGREGATOR,
                },
                ..Default::default()
            })?)
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
        } = builder.launch().await?;

        let test_ctx = NodeTestContext::new(node, optimism_payload_attributes).await?;
        Ok(Self {
            signers: (0..5),
            tasks,
            node: test_ctx,
        })
    }

    pub async fn raw_pbh_tx_bytes(
        &self,
        acc: u32,
        pbh_nonce: u8,
        tx_nonce: u64,
        user_op_nonce: U256,
    ) -> Bytes {
        PBHTransactionTestContext::raw_pbh_tx_bytes(acc, pbh_nonce, tx_nonce, user_op_nonce).await
    }
}

#[tokio::test]
async fn test_can_build_pbh_payload() -> eyre::Result<()> {
    let mut ctx = WorldChainBuilderTestContext::setup().await?;
    let mut pbh_tx_hashes = vec![];
    let signers = ctx.signers.clone();
    for signer in signers.into_iter() {
        let raw_tx = ctx.raw_pbh_tx_bytes(signer, 0, 0, U256::ZERO).await;
        let pbh_hash = ctx.node.rpc.inject_tx(raw_tx.clone()).await?;
        pbh_tx_hashes.push(pbh_hash);
    }

    let (payload, _) = ctx.node.advance_block().await?;

    assert_eq!(payload.block().body.transactions.len(), pbh_tx_hashes.len());
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
        let raw_tx = ctx.raw_pbh_tx_bytes(signer.clone(), 0, 0, U256::ZERO).await;
        let pbh_hash = ctx.node.rpc.inject_tx(raw_tx.clone()).await?;
        pbh_tx_hashes.push(pbh_hash);
    }

    let (payload, _) = ctx.node.advance_block().await?;

    assert_eq!(
        payload.block().body.transactions.len(),
        pbh_tx_hashes.len() + 1
    );
    // Assert the non-pbh transaction is included in the block last
    assert_eq!(
        payload.block().body.transactions.last().unwrap().hash(),
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
    let raw_tx = ctx.raw_pbh_tx_bytes(signer.clone(), 0, 0, U256::ZERO).await;
    ctx.node.rpc.inject_tx(raw_tx.clone()).await?;
    let dup_pbh_hash_res = ctx.node.rpc.inject_tx(raw_tx.clone()).await;
    assert!(dup_pbh_hash_res.is_err());
    Ok(())
}

#[tokio::test]
async fn test_dup_pbh_nonce() -> eyre::Result<()> {
    let mut ctx = WorldChainBuilderTestContext::setup().await?;
    let signer = 0;

    let raw_tx_0 = ctx.raw_pbh_tx_bytes(signer.clone(), 0, 0, U256::ZERO).await;
    ctx.node.rpc.inject_tx(raw_tx_0.clone()).await?;
    let raw_tx_1 = ctx.raw_pbh_tx_bytes(signer.clone(), 0, 0, U256::ZERO).await;

    // Now that the nullifier has successfully been stored in
    // the `ExecutedPbhNullifierTable`, inserting a new tx with the
    // same pbh_nonce should fail to validate.
    assert!(ctx.node.rpc.inject_tx(raw_tx_1.clone()).await.is_err());

    let (payload, _) = ctx.node.advance_block().await?;

    // One transaction should be successfully validated
    // and included in the block.
    assert_eq!(payload.block().body.transactions.len(), 1);

    Ok(())
}

/// Helper function to create a new eth payload attributes
pub fn optimism_payload_attributes(timestamp: u64) -> OpPayloadBuilderAttributes {
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
    genesis.config.chain_id = DEV_CHAIN_ID;

    OpChainSpecBuilder::base_mainnet()
        .genesis(genesis.extend_accounts(vec![(
            OP_WORLD_ID,
            GenesisAccount::default().with_storage(Some(BTreeMap::from_iter(vec![(
                LATEST_ROOT_SLOT.into(),
                tree_root().into(),
            )]))),
        )]))
        .ecotone_activated()
        .build()
}
