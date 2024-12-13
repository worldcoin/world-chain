//! Utilities for running world chain builder end-to-end tests.
use crate::{
    node::{
        args::{ExtArgs, WorldChainBuilderArgs},
        builder::WorldChainBuilder,
    },
    pbh::{
        date_marker::DateMarker,
        external_nullifier::{ExternalNullifier, Prefix},
        payload::{PbhPayload, Proof},
    },
    pool::{
        ordering::WorldChainOrdering,
        root::{LATEST_ROOT_SLOT, OP_WORLD_ID},
        tx::WorldChainPooledTransaction,
        validator::WorldChainTransactionValidator,
    },
    primitives::WorldChainPooledTransactionsElement,
    rpc::bundle::{EthTransactionsExtServer, WorldChainEthApiExt},
};
use alloy_eips::eip2718::Decodable2718;
use alloy_genesis::{Genesis, GenesisAccount};
use alloy_network::eip2718::Encodable2718;
use alloy_network::{Ethereum, EthereumWallet, TransactionBuilder};
use alloy_rpc_types::{TransactionInput, TransactionRequest, Withdrawals};
use alloy_signer_local::PrivateKeySigner;
use chrono::Utc;
use reth::payload::{EthPayloadBuilderAttributes, PayloadId};
use reth::tasks::TaskManager;
use reth::transaction_pool::{blobstore::DiskFileBlobStore, TransactionValidationTaskExecutor};
use reth::{
    api::{FullNodeTypesAdapter, NodeTypesWithDBAdapter},
    builder::components::Components,
};
use reth::{
    builder::{NodeAdapter, NodeBuilder, NodeConfig, NodeHandle},
    transaction_pool::Pool,
};
use reth_db::{
    test_utils::{tempdir_path, TempDatabase},
    DatabaseEnv,
};
use reth_e2e_test_utils::{
    node::NodeTestContext, transaction::TransactionTestContext, wallet::Wallet,
};
use reth_evm::execute::BasicBlockExecutorProvider;
use reth_node_core::args::RpcServerArgs;
use reth_optimism_chainspec::{OpChainSpec, OpChainSpecBuilder};
use reth_optimism_consensus::OpBeaconConsensus;
use reth_optimism_evm::{OpEvmConfig, OpExecutionStrategyFactory};
use reth_optimism_node::{node::OpAddOns, OpPayloadBuilderAttributes};
use reth_primitives::PooledTransactionsElement;
use reth_provider::providers::BlockchainProvider;
use revm_primitives::{Address, Bytes, FixedBytes, TxKind, B256, U256};
use semaphore::{
    hash_to_field,
    identity::Identity,
    poseidon_tree::LazyPoseidonTree,
    protocol::{generate_nullifier_hash, generate_proof},
    Field,
};
use serial_test::serial;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

pub const DEV_CHAIN_ID: u64 = 8453;

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
    pub pbh_wallets: Vec<PrivateKeySigner>,
    pub tree: LazyPoseidonTree,
    pub tasks: TaskManager,
    pub node: Adapter,
    pub identities: HashMap<Address, usize>,
}

impl WorldChainBuilderTestContext {
    pub async fn setup() -> eyre::Result<Self> {
        let wallets = Wallet::new(20).with_chain_id(DEV_CHAIN_ID).gen();
        let mut tree = LazyPoseidonTree::new(30, Field::from(0)).derived();
        let mut identities = HashMap::new();
        for (i, signer) in wallets.iter().enumerate() {
            let address = signer.address();
            identities.insert(address, i);
            let identity = Identity::from_secret(signer.address().as_mut_slice(), None);
            tree = tree.update(i, &identity.commitment());
        }

        let op_chain_spec = Arc::new(get_chain_spec(tree.root()));

        let tasks = TaskManager::current();
        let exec = tasks.executor();

        let node_config: NodeConfig<OpChainSpec> = NodeConfig::new(op_chain_spec.clone())
            .with_chain(op_chain_spec.clone())
            .with_unused_ports()
            .with_rpc(
                RpcServerArgs::default()
                    .with_unused_ports()
                    .with_http_unused_port(),
            );
        let path = tempdir_path();
        let NodeHandle {
            node,
            node_exit_future: _,
        } = NodeBuilder::new(node_config.clone())
            .testing_node(exec.clone())
            .node(WorldChainBuilder::new(
                ExtArgs {
                    builder_args: WorldChainBuilderArgs {
                        num_pbh_txs: 30,
                        verified_blockspace_capacity: 70,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                &path,
            )?)
            .extend_rpc_modules(move |ctx| {
                let provider = ctx.provider().clone();
                let pool = ctx.pool().clone();
                let eth_api_ext = WorldChainEthApiExt::new(pool, provider);
                ctx.modules.merge_configured(eth_api_ext.into_rpc())?;
                Ok(())
            })
            .launch()
            .await?;
        let test_ctx = NodeTestContext::new(node, optimism_payload_attributes).await?;
        Ok(Self {
            pbh_wallets: wallets,
            tree,
            tasks,
            node: test_ctx,
            identities,
        })
    }

    pub async fn raw_pbh_tx_bytes(
        &self,
        signer: PrivateKeySigner,
        pbh_nonce: u16,
        tx_nonce: u64,
    ) -> Bytes {
        let tx = tx(DEV_CHAIN_ID, None, tx_nonce);
        let envelope = TransactionTestContext::sign_tx(signer.clone(), tx).await;
        let raw_tx = envelope.encoded_2718();
        let mut data = raw_tx.as_ref();
        let recovered = PooledTransactionsElement::decode_2718(&mut data).unwrap();
        let pbh_payload = self.valid_proof(
            signer.address(),
            recovered.hash().as_slice(),
            chrono::Utc::now(),
            pbh_nonce,
        );

        let world_chain_pooled_tx_element = WorldChainPooledTransactionsElement {
            inner: recovered,
            pbh_payload: Some(pbh_payload.clone()),
        };

        let mut buff = Vec::<u8>::new();
        world_chain_pooled_tx_element.encode_2718(&mut buff);
        buff.into()
    }

    fn valid_proof(
        &self,
        identity: Address,
        tx_hash: &[u8],
        time: chrono::DateTime<Utc>,
        pbh_nonce: u16,
    ) -> PbhPayload {
        let external_nullifier =
            ExternalNullifier::new(Prefix::V1, DateMarker::from(time), pbh_nonce).to_string();

        self.create_proof(identity, external_nullifier, tx_hash)
    }

    fn create_proof(
        &self,
        mut identity: Address,
        external_nullifier: String,
        signal: &[u8],
    ) -> PbhPayload {
        let idx = self.identities.get(&identity).unwrap();
        let secret = identity.as_mut_slice();
        // generate identity
        let id = Identity::from_secret(secret, None);
        let merkle_proof = self.tree.proof(*idx);

        let signal_hash = hash_to_field(signal);
        let external_nullifier_hash = hash_to_field(external_nullifier.as_bytes());
        let nullifier_hash = generate_nullifier_hash(&id, external_nullifier_hash);

        let proof = Proof(
            generate_proof(&id, &merkle_proof, external_nullifier_hash, signal_hash).unwrap(),
        );

        PbhPayload {
            root: self.tree.root(),
            nullifier_hash,
            external_nullifier,
            proof,
        }
    }
}

#[tokio::test]
#[serial]
async fn test_can_build_pbh_payload() -> eyre::Result<()> {
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut ctx = WorldChainBuilderTestContext::setup().await?;
    let mut pbh_tx_hashes = vec![];
    for signer in ctx.pbh_wallets.iter() {
        let raw_tx = ctx.raw_pbh_tx_bytes(signer.clone(), 0, 0).await;
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
#[serial]
async fn test_transaction_pool_ordering() -> eyre::Result<()> {
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut ctx = WorldChainBuilderTestContext::setup().await?;
    let non_pbh_tx = tx(ctx.node.inner.chain_spec().chain.id(), None, 0);
    let wallet = ctx.pbh_wallets[0].clone();
    let signer = EthereumWallet::from(wallet);
    let signed = <TransactionRequest as TransactionBuilder<Ethereum>>::build(non_pbh_tx, &signer)
        .await
        .unwrap();
    let non_pbh_hash = ctx.node.rpc.inject_tx(signed.encoded_2718().into()).await?;
    let mut pbh_tx_hashes = vec![];
    for signer in ctx.pbh_wallets.iter().skip(1) {
        let raw_tx = ctx.raw_pbh_tx_bytes(signer.clone(), 0, 0).await;
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
#[serial]
async fn test_invalidate_dup_tx_and_nullifier() -> eyre::Result<()> {
    tokio::time::sleep(Duration::from_secs(1)).await;
    let ctx = WorldChainBuilderTestContext::setup().await?;
    let signer = ctx.pbh_wallets[0].clone();
    let raw_tx = ctx.raw_pbh_tx_bytes(signer.clone(), 0, 0).await;
    ctx.node.rpc.inject_tx(raw_tx.clone()).await?;
    let dup_pbh_hash_res = ctx.node.rpc.inject_tx(raw_tx.clone()).await;
    assert!(dup_pbh_hash_res.is_err());
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_dup_pbh_nonce() -> eyre::Result<()> {
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut ctx = WorldChainBuilderTestContext::setup().await?;
    let signer = ctx.pbh_wallets[0].clone();

    let raw_tx_0 = ctx.raw_pbh_tx_bytes(signer.clone(), 0, 0).await;
    ctx.node.rpc.inject_tx(raw_tx_0.clone()).await?;
    let raw_tx_1 = ctx.raw_pbh_tx_bytes(signer.clone(), 0, 1).await;

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

fn tx(chain_id: u64, data: Option<Bytes>, nonce: u64) -> TransactionRequest {
    TransactionRequest {
        nonce: Some(nonce),
        value: Some(U256::from(100)),
        to: Some(TxKind::Call(Address::random())),
        gas: Some(210000),
        max_fee_per_gas: Some(20e10 as u128),
        max_priority_fee_per_gas: Some(20e10 as u128),
        chain_id: Some(chain_id),
        input: TransactionInput { input: None, data },
        ..Default::default()
    }
}

/// Builds an OP Mainnet chain spec with the given merkle root
/// Populated in the OpWorldID contract.
fn get_chain_spec(merkle_root: Field) -> OpChainSpec {
    let genesis: Genesis = serde_json::from_str(include_str!("assets/genesis.json")).unwrap();
    OpChainSpecBuilder::base_mainnet()
        .genesis(genesis.extend_accounts(vec![(
            OP_WORLD_ID,
            GenesisAccount::default().with_storage(Some(BTreeMap::from_iter(vec![(
                LATEST_ROOT_SLOT.into(),
                merkle_root.into(),
            )]))),
        )]))
        .ecotone_activated()
        .build()
}
