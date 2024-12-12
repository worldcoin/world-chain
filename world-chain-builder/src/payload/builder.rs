use alloy_consensus::EMPTY_OMMER_ROOT_HASH;
use alloy_eips::merge::BEACON_NONCE;
use reth_evm::ConfigureEvm;
use reth_optimism_node::OpPayloadBuilder;
use reth_optimism_payload_builder::config::OpBuilderConfig;
use std::sync::Arc;

use reth::api::PayloadBuilderError;
use reth::builder::components::PayloadServiceBuilder;
use reth::builder::{BuilderContext, FullNodeTypes, NodeTypesWithEngine, PayloadBuilderConfig};
use reth::chainspec::EthereumHardforks;
use reth::payload::{PayloadBuilderHandle, PayloadBuilderService};
use reth::revm::database::StateProviderDatabase;
use reth::revm::db::states::bundle_state::BundleRetention;
use reth::revm::DatabaseCommit;
use reth::revm::State;
use reth::transaction_pool::{BestTransactionsAttributes, TransactionPool};
use reth_basic_payload_builder::{
    commit_withdrawals, is_better_payload, BasicPayloadJobGenerator,
    BasicPayloadJobGeneratorConfig, BuildArguments, BuildOutcome, MissingPayloadBehaviour,
    PayloadBuilder, PayloadConfig,
};
use reth_chain_state::ExecutedBlock;
use reth_db::DatabaseEnv;
use reth_evm::system_calls::SystemCaller;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_consensus::calculate_receipt_root_no_memo_optimism;
use reth_primitives::{proofs, BlockBody};
use reth_primitives::{Block, Header, Receipt, TxType};
use reth_provider::{
    BlockReaderIdExt, CanonStateSubscriptions, ChainSpecProvider, ExecutionOutcome,
    StateProviderFactory,
};
use reth_trie::HashedPostState;
use revm_primitives::calc_excess_blob_gas;
use revm_primitives::{
    BlockEnv, CfgEnvWithHandlerCfg, EVMError, EnvWithHandlerCfg, InvalidTransaction,
    ResultAndState, U256,
};
use tracing::{debug, trace, warn};

use crate::pool::noop::NoopWorldChainTransactionPool;
use crate::pool::tx::WorldChainPoolTransaction;
use crate::rpc::bundle::validate_conditional_options;

/// World Chain payload builder
#[derive(Debug)]
pub struct WorldChainPayloadBuilder<EvmConfig> {
    pub inner: OpPayloadBuilder<EvmConfig>,
    pub verified_blockspace_capacity: u8,
}

impl<EvmConfig> WorldChainPayloadBuilder<EvmConfig>
where
    EvmConfig: ConfigureEvm<Header = Header>,
{
    /// `OptimismPayloadBuilder` constructor.
    pub const fn new(
        evm_config: EvmConfig,
        builder_config: OpBuilderConfig,
        verified_blockspace_capacity: u8,
    ) -> Self {
        let inner = OpPayloadBuilder::with_builder_config(evm_config, builder_config);

        Self {
            inner,
            verified_blockspace_capacity,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        node::test_utils::{WorldChainNoopProvider, WorldChainNoopValidator},
        pool::{
            ordering::WorldChainOrdering, root::WorldChainRootValidator,
            tx::WorldChainPooledTransaction, validator::WorldChainTransactionValidator,
        },
        test::get_pbh_transaction,
    };

    use super::*;
    use crate::pbh::db::load_world_chain_db;
    use alloy_consensus::TxLegacy;
    use alloy_primitives::Parity;
    use alloy_rlp::Encodable;
    use op_alloy_consensus::TxDeposit;
    use rand::Rng;
    use reth::chainspec::ChainSpec;
    use reth::payload::{EthPayloadBuilderAttributes, PayloadId};
    use reth::transaction_pool::{
        blobstore::DiskFileBlobStore, validate::EthTransactionValidatorBuilder,
        EthPooledTransaction, PoolConfig, PoolTransaction, TransactionOrigin,
    };
    use reth_db::test_utils::tempdir_path;
    use reth_optimism_chainspec::OpChainSpec;
    use reth_optimism_evm::OptimismEvmConfig;
    use reth_optimism_node::txpool::OpTransactionValidator;
    use reth_primitives::{
        transaction::WithEncoded, SealedBlock, Signature, TransactionSigned,
        TransactionSignedEcRecovered, Withdrawals,
    };
    use revm_primitives::{ruint::aliases::U256, Address, Bytes, TxKind, B256};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_try_build() -> eyre::Result<()> {
        let data_dir = tempdir_path();
        let db = load_world_chain_db(data_dir.as_path(), false)?;

        let gas_limit = 30_000_000;
        let chain_spec = Arc::new(ChainSpec::default());
        let evm_config = OptimismEvmConfig::new(Arc::new(OpChainSpec {
            inner: (*chain_spec).clone(),
        }));
        let blob_store = DiskFileBlobStore::open(data_dir.as_path(), Default::default())?;

        // Init the transaction pool
        let client = WorldChainNoopProvider::default();
        let eth_tx_validator = EthTransactionValidatorBuilder::new(chain_spec.clone())
            .build(client, blob_store.clone());
        let op_tx_validator =
            OpTransactionValidator::new(eth_tx_validator).require_l1_data_gas_fee(false);
        let root_validator = WorldChainRootValidator::new(client);

        let wc_validator = WorldChainTransactionValidator::new(
            op_tx_validator,
            root_validator.unwrap(),
            db.clone(),
            30,
        );

        let wc_noop_validator = WorldChainNoopValidator::new(wc_validator);
        let ordering = WorldChainOrdering::default();

        let world_chain_tx_pool = reth::transaction_pool::Pool::new(
            wc_noop_validator,
            ordering,
            blob_store,
            PoolConfig::default(),
        );

        // Init the payload builder
        let verified_blockspace_cap = 50;
        let world_chain_payload_builder =
            WorldChainPayloadBuilder::new(evm_config, verified_blockspace_cap);

        // Insert transactions into the pool
        let unverified_transactions = generate_mock_pooled_transactions(50, 100000, false);
        for transaction in unverified_transactions.iter() {
            world_chain_tx_pool
                .add_transaction(TransactionOrigin::Local, transaction.clone())
                .await?;
        }

        // Insert verifiedtransactions into the pool
        let verified_transactions = generate_mock_pooled_transactions(50, 100000, true);
        for transaction in verified_transactions.iter() {
            world_chain_tx_pool
                .add_transaction(TransactionOrigin::Local, transaction.clone())
                .await?;
        }

        let sequencer_transactions = generate_mock_deposit_transactions(50, 100000);

        let eth_payload_attributes = EthPayloadBuilderAttributes {
            id: PayloadId::new([0; 8]),
            parent: B256::ZERO,
            timestamp: 0,
            suggested_fee_recipient: Address::ZERO,
            prev_randao: B256::ZERO,
            withdrawals: Withdrawals::default(),
            parent_beacon_block_root: None,
        };

        let payload_attributes = OptimismPayloadBuilderAttributes {
            gas_limit: Some(gas_limit),
            transactions: sequencer_transactions.clone(),
            payload_attributes: eth_payload_attributes,
            no_tx_pool: false,
        };

        let build_args = BuildArguments {
            client: WorldChainNoopProvider::default(),
            config: PayloadConfig {
                parent_block: Arc::new(SealedBlock::default()),
                attributes: payload_attributes,
                // chain_spec,
                extra_data: Bytes::default(),
            },
            pool: world_chain_tx_pool,
            cached_reads: Default::default(),
            cancel: Default::default(),
            best_payload: None,
        };

        let built_payload = world_chain_payload_builder
            .try_build(build_args)?
            .into_payload()
            .expect("Could not build payload");

        // Collect the transaction hashes in the expected order
        let mut expected_order = sequencer_transactions
            .iter()
            .map(|tx| tx.1.hash())
            .collect::<Vec<_>>();
        expected_order.extend(verified_transactions.iter().map(|tx| tx.hash()));
        expected_order.extend(unverified_transactions.iter().map(|tx| tx.hash()));

        for (tx, expected_hash) in built_payload
            .block()
            .body
            .transactions
            .iter()
            .zip(expected_order.iter())
        {
            assert_eq!(tx.hash, *expected_hash);
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_try_build_max_verified_blockspace() -> eyre::Result<()> {
        let data_dir = tempdir_path();
        let db = load_world_chain_db(data_dir.as_path(), false)?;

        let gas_limit = 30_000_000;
        let chain_spec = Arc::new(ChainSpec::default());
        let evm_config = OptimismEvmConfig::new(Arc::new(OpChainSpec {
            inner: (*chain_spec).clone(),
        }));
        let blob_store = DiskFileBlobStore::open(data_dir.as_path(), Default::default())?;

        // Init the transaction pool
        let client = WorldChainNoopProvider::default();
        let eth_tx_validator = EthTransactionValidatorBuilder::new(chain_spec.clone())
            .build(client, blob_store.clone());
        let op_tx_validator =
            OpTransactionValidator::new(eth_tx_validator).require_l1_data_gas_fee(false);
        let root_validator = WorldChainRootValidator::new(client);
        let wc_validator = WorldChainTransactionValidator::new(
            op_tx_validator,
            root_validator.unwrap(),
            db.clone(),
            30,
        );

        let wc_noop_validator = WorldChainNoopValidator::new(wc_validator);
        let ordering = WorldChainOrdering::default();

        let world_chain_tx_pool = reth::transaction_pool::Pool::new(
            wc_noop_validator,
            ordering,
            blob_store,
            PoolConfig::default(),
        );

        // Init the payload builder
        let verified_blockspace_cap = 10;
        let world_chain_payload_builder =
            WorldChainPayloadBuilder::new(evm_config, verified_blockspace_cap);

        // Insert transactions into the pool
        let unverified_transactions = generate_mock_pooled_transactions(50, 100000, false);
        for transaction in unverified_transactions.iter() {
            world_chain_tx_pool
                .add_transaction(TransactionOrigin::Local, transaction.clone())
                .await?;
        }

        // Insert verifiedtransactions into the pool
        let verified_transactions = generate_mock_pooled_transactions(50, 3000000, true);
        for transaction in verified_transactions.iter() {
            world_chain_tx_pool
                .add_transaction(TransactionOrigin::Local, transaction.clone())
                .await?;
        }

        let sequencer_transactions = generate_mock_deposit_transactions(50, 100000);

        let eth_payload_attributes = EthPayloadBuilderAttributes {
            id: PayloadId::new([0; 8]),
            parent: B256::ZERO,
            timestamp: 0,
            suggested_fee_recipient: Address::ZERO,
            prev_randao: B256::ZERO,
            withdrawals: Withdrawals::default(),
            parent_beacon_block_root: None,
        };

        let payload_attributes = OptimismPayloadBuilderAttributes {
            gas_limit: Some(gas_limit),
            transactions: sequencer_transactions.clone(),
            payload_attributes: eth_payload_attributes,
            no_tx_pool: false,
        };

        let build_args = BuildArguments {
            client: WorldChainNoopProvider::default(),
            config: PayloadConfig {
                parent_block: Arc::new(SealedBlock::default()),
                attributes: payload_attributes,
                // chain_spec,
                extra_data: Bytes::default(),
            },
            pool: world_chain_tx_pool,
            cached_reads: Default::default(),
            cancel: Default::default(),
            best_payload: None,
        };

        let built_payload = world_chain_payload_builder
            .try_build(build_args)?
            .into_payload()
            .expect("Could not build payload");

        // Collect the transaction hashes in the expected order
        let mut expected_order = sequencer_transactions
            .iter()
            .map(|tx| tx.1.hash())
            .collect::<Vec<_>>();
        expected_order.push(*verified_transactions.first().unwrap().hash());
        expected_order.extend(unverified_transactions.iter().map(|tx| tx.hash()));

        for (tx, expected_hash) in built_payload
            .block()
            .body
            .transactions
            .iter()
            .zip(expected_order.iter())
        {
            assert_eq!(tx.hash, *expected_hash);
        }

        Ok(())
    }

    fn generate_mock_deposit_transactions(
        count: usize,
        gas_limit: u64,
    ) -> Vec<WithEncoded<TransactionSigned>> {
        let mut rng = rand::thread_rng();

        (0..count)
            .map(|_| {
                let tx = reth_primitives::Transaction::Deposit(TxDeposit {
                    source_hash: B256::random(),
                    from: Address::random(),
                    to: TxKind::Call(Address::random()),
                    mint: Some(100), // Example value for mint
                    value: U256::from(100),
                    gas_limit,
                    is_system_transaction: true,
                    input: rng.gen::<[u8; 32]>().into(),
                });

                let signature = Signature::new(
                    U256::from(rng.gen::<u128>()),
                    U256::from(rng.gen::<u128>()),
                    Parity::Parity(false),
                );

                let tx = TransactionSigned::from_transaction_and_signature(tx, signature);
                let mut buf = Vec::new();
                tx.encode(&mut buf);
                WithEncoded::new(buf.into(), tx)
            })
            .collect::<Vec<_>>()
    }

    fn generate_mock_pooled_transactions(
        count: usize,
        gas_limit: u64,
        pbh: bool,
    ) -> Vec<WorldChainPooledTransaction> {
        let mut rng = rand::thread_rng();

        (0..count)
            .map(|i| {
                let tx = reth_primitives::Transaction::Legacy(TxLegacy {
                    gas_price: 10,
                    gas_limit,
                    to: TxKind::Call(Address::random()),
                    value: U256::from(100),
                    input: rng.gen::<[u8; 32]>().into(),
                    nonce: rng.gen(),
                    ..Default::default()
                });

                let signature = Signature::new(
                    U256::from(rng.gen::<u128>()),
                    U256::from(rng.gen::<u128>()),
                    Parity::Parity(false),
                );

                let tx = TransactionSigned::from_transaction_and_signature(tx, signature);
                let tx_recovered = TransactionSignedEcRecovered::from_signed_transaction(
                    tx.clone(),
                    Default::default(),
                );
                let pooled_tx = EthPooledTransaction::new(tx_recovered.clone(), 200);

                let pbh_payload = if pbh {
                    Some(get_pbh_transaction(i as u16).pbh_payload.unwrap())
                } else {
                    None
                };

                WorldChainPooledTransaction {
                    inner: pooled_tx,
                    pbh_payload,
                    conditional_options: None,
                }
            })
            .collect::<Vec<_>>()
    }
}
