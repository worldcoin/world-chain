//! Utilities for running world chain builder end-to-end tests.
use crate::{
    date_marker::DateMarker,
    external_nullifier::ExternalNullifier,
    node::{
        args::{ExtArgs, WorldChainBuilderArgs},
        builder::{WorldChainAddOns, WorldChainBuilder},
    },
    pbh::semaphore::{Proof, SemaphoreProof},
    pool::{
        ordering::WorldChainOrdering,
        root::{LATEST_ROOT_SLOT, OP_WORLD_ID},
        tx::WorldChainPooledTransaction,
        validator::WorldChainTransactionValidator,
    },
    primitives::WorldChainPooledTransactionsElement,
};
use alloy_genesis::{Genesis, GenesisAccount};
use alloy_network::eip2718::Encodable2718;
use alloy_network::{Ethereum, EthereumWallet, TransactionBuilder};
use alloy_rpc_types::{TransactionInput, TransactionRequest};
use alloy_signer_local::PrivateKeySigner;
use chrono::Utc;
use reth_chainspec::ChainSpec;
use reth_consensus::Consensus;
use reth_db::{
    test_utils::{tempdir_path, TempDatabase},
    DatabaseEnv,
};
use reth_e2e_test_utils::{
    node::NodeTestContext, transaction::TransactionTestContext, wallet::Wallet,
};
use reth_node_api::{FullNodeTypesAdapter, NodeTypesWithDBAdapter};
use reth_node_builder::{components::Components, NodeAdapter, NodeBuilder, NodeConfig, NodeHandle};
use reth_node_core::args::RpcServerArgs;
use reth_optimism_chainspec::{OpChainSpec, BASE_MAINNET};
use reth_optimism_evm::{OpExecutorProvider, OptimismEvmConfig};
use reth_optimism_node::{engine::OptimismEngineValidator, OptimismPayloadBuilderAttributes};
use reth_payload_builder::{EthPayloadBuilderAttributes, PayloadId};
use reth_primitives::{PooledTransactionsElement, Withdrawals};
use reth_provider::providers::BlockchainProvider;
use reth_tasks::TaskManager;
use reth_transaction_pool::{
    blobstore::DiskFileBlobStore, Pool, TransactionValidationTaskExecutor,
};
use revm_primitives::{Address, Bytes, FixedBytes, TxKind, B256, U256};
use semaphore::{
    hash_to_field,
    identity::Identity,
    poseidon_tree::LazyPoseidonTree,
    protocol::{generate_nullifier_hash, generate_proof},
    Field,
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

pub const DEV_CHAIN_ID: u64 = 8453;

type Adapter = NodeAdapter<
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
        OptimismEvmConfig,
        OpExecutorProvider,
        Arc<dyn Consensus>,
        OptimismEngineValidator,
    >,
>;

pub struct WorldChainBuilderTestContext {
    pub pbh_wallets: Vec<PrivateKeySigner>,
    pub tree: LazyPoseidonTree,
    pub node: NodeTestContext<Adapter, WorldChainAddOns>,
    pub tasks: TaskManager,
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

        let op_chain_spec = Arc::new(OpChainSpec {
            inner: get_chain_spec(tree.root()),
        });

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
            .launch()
            .await?;

        Ok(Self {
            pbh_wallets: wallets,
            tree,
            node: NodeTestContext::new(node).await?,
            tasks,
            identities,
        })
    }

    pub async fn raw_pbh_tx_bytes(&self, signer: PrivateKeySigner, pbh_nonce: u16) -> Bytes {
        let raw_tx = TransactionTestContext::transfer_tx_bytes(
            self.node.inner.chain_spec().chain.id(),
            signer.clone(),
        )
        .await;

        let mut data = raw_tx.as_ref();
        let recovered = PooledTransactionsElement::decode_enveloped(&mut data).unwrap();
        let proof = self.valid_proof(
            signer.address(),
            recovered.hash().as_slice(),
            chrono::Utc::now(),
            pbh_nonce,
        );

        let world_chain_pooled_tx_element = WorldChainPooledTransactionsElement {
            inner: recovered,
            semaphore_proof: Some(proof.clone()),
        };

        let mut buff = Vec::<u8>::new();
        world_chain_pooled_tx_element.encode_enveloped(&mut buff);
        buff.into()
    }

    fn valid_proof(
        &self,
        identity: Address,
        tx_hash: &[u8],
        time: chrono::DateTime<Utc>,
        pbh_nonce: u16,
    ) -> SemaphoreProof {
        let external_nullifier =
            ExternalNullifier::new(DateMarker::from(time), pbh_nonce).to_string();

        self.create_proof(identity, external_nullifier, tx_hash)
    }

    fn create_proof(
        &self,
        mut identity: Address,
        external_nullifier: String,
        signal: &[u8],
    ) -> SemaphoreProof {
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

        SemaphoreProof {
            root: self.tree.root(),
            nullifier_hash,
            signal_hash,
            external_nullifier,
            proof,
            external_nullifier_hash,
        }
    }
}

#[tokio::test]
async fn test_can_build_pbh_payload() -> eyre::Result<()> {
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut ctx = WorldChainBuilderTestContext::setup().await?;
    let mut pbh_tx_hashes = vec![];
    for signer in ctx.pbh_wallets.iter() {
        let raw_tx = ctx.raw_pbh_tx_bytes(signer.clone(), 0).await;
        let pbh_hash = ctx.node.rpc.inject_tx(raw_tx.clone()).await?;
        pbh_tx_hashes.push(pbh_hash);
    }

    let (payload, _) = ctx
        .node
        .advance_block(vec![], optimism_payload_attributes)
        .await?;

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
        let raw_tx = ctx.raw_pbh_tx_bytes(signer.clone(), 0).await;
        let pbh_hash = ctx.node.rpc.inject_tx(raw_tx.clone()).await?;
        pbh_tx_hashes.push(pbh_hash);
    }

    let (payload, _) = ctx
        .node
        .advance_block(vec![], optimism_payload_attributes)
        .await?;

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
    tokio::time::sleep(Duration::from_secs(1)).await;
    let ctx = WorldChainBuilderTestContext::setup().await?;
    let signer = ctx.pbh_wallets[0].clone();
    let raw_tx = ctx.raw_pbh_tx_bytes(signer.clone(), 0).await;
    ctx.node.rpc.inject_tx(raw_tx.clone()).await?;
    let dup_pbh_hash_res = ctx.node.rpc.inject_tx(raw_tx.clone()).await;
    assert!(dup_pbh_hash_res.is_err());
    Ok(())
}

/// Helper function to create a new eth payload attributes
pub fn optimism_payload_attributes(timestamp: u64) -> OptimismPayloadBuilderAttributes {
    let attributes = EthPayloadBuilderAttributes {
        timestamp,
        prev_randao: B256::ZERO,
        suggested_fee_recipient: Address::ZERO,
        withdrawals: Withdrawals::default(),
        parent_beacon_block_root: Some(B256::ZERO),
        id: PayloadId(FixedBytes::<8>::random()),
        parent: FixedBytes::default(),
    };

    OptimismPayloadBuilderAttributes {
        payload_attributes: attributes,
        transactions: vec![],
        gas_limit: None,
        no_tx_pool: false,
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
fn get_chain_spec(merkle_root: Field) -> ChainSpec {
    let genesis: Genesis = serde_json::from_str(include_str!("assets/genesis.json")).unwrap();
    ChainSpec::builder()
        .chain(BASE_MAINNET.chain)
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
