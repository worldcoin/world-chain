use std::sync::Arc;

use reth_basic_payload_builder::{
    commit_withdrawals, is_better_payload, BasicPayloadJobGenerator,
    BasicPayloadJobGeneratorConfig, BuildArguments, BuildOutcome, MissingPayloadBehaviour,
    PayloadBuilder, PayloadConfig, WithdrawalsOutcome,
};
use reth_chain_state::ExecutedBlock;
use reth_chainspec::{ChainSpec, EthereumHardforks, OptimismHardfork};
use reth_db::cursor::DbCursorRW;
use reth_db::mdbx::tx::Tx;
use reth_db::mdbx::RW;
use reth_db::transaction::{DbTx, DbTxMut};
use reth_db::{DatabaseEnv, DatabaseError};
use reth_evm::system_calls::pre_block_beacon_root_contract_call;
use reth_evm::ConfigureEvm;
use reth_evm_optimism::OptimismEvmConfig;
use reth_node_builder::components::PayloadServiceBuilder;
use reth_node_builder::{BuilderContext, FullNodeTypes, NodeTypesWithEngine, PayloadBuilderConfig};
use reth_node_optimism::{
    OptimismBuiltPayload, OptimismEngineTypes, OptimismPayloadBuilder,
    OptimismPayloadBuilderAttributes,
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_payload_builder::error::OptimismPayloadBuilderError;
use reth_payload_builder::error::PayloadBuilderError;
use reth_payload_builder::{PayloadBuilderHandle, PayloadBuilderService};
use reth_primitives::constants::BEACON_NONCE;
use reth_primitives::eip4844::calculate_excess_blob_gas;
use reth_primitives::{proofs, IntoRecoveredTransaction};
use reth_primitives::{Block, Header, Receipt, TxHash, TxType, EMPTY_OMMER_ROOT_HASH};
use reth_provider::{CanonStateSubscriptions, ExecutionOutcome, StateProviderFactory};
use reth_revm::database::StateProviderDatabase;
use reth_revm::db::states::bundle_state::BundleRetention;
use reth_revm::DatabaseCommit;
use reth_revm::State;
use reth_transaction_pool::{BestTransactionsAttributes, TransactionPool};
use reth_trie::HashedPostState;
use revm_primitives::{
    BlockEnv, CfgEnvWithHandlerCfg, EVMError, EnvWithHandlerCfg, FixedBytes, InvalidTransaction,
    ResultAndState, U256,
};
use tracing::{debug, trace, warn};

use crate::node::builder::load_world_chain_db;
use crate::pbh::db::{EmptyValue, ExecutedPbhNullifierTable, ValidatedPbhTransactionTable};
use crate::pool::noop::NoopWorldChainTransactionPool;
use crate::pool::tx::WorldChainPoolTransaction;

/// Priority blockspace for humans builder
#[derive(Debug, Clone)]
pub struct WorldChainPayloadBuilder<EvmConfig> {
    inner: OptimismPayloadBuilder<EvmConfig>,
    // TODO: docs describing that this is a percent, ex: 50 is 50/100
    verified_blockspace_capacity: u64,
    // TODO: NOTE: we need to insert the verified txs into the table after they are inserted into the block
    _database_env: Arc<DatabaseEnv>,
}

impl<EvmConfig> WorldChainPayloadBuilder<EvmConfig>
where
    EvmConfig: ConfigureEvm<Header = Header>,
{
    /// `OptimismPayloadBuilder` constructor.
    pub const fn new(
        evm_config: EvmConfig,
        verified_blockspace_capacity: u64,
        _database_env: Arc<DatabaseEnv>,
    ) -> Self {
        let inner = OptimismPayloadBuilder::new(evm_config);

        Self {
            inner,
            verified_blockspace_capacity,
            _database_env,
        }
    }

    /// Check the database to see if a tx has been validated for PBH
    /// If so return the nullifier
    pub fn get_pbh_validated(
        &self,
        db_tx: Tx<RW>,
        tx: TxHash,
    ) -> Result<Option<FixedBytes<32>>, DatabaseError> {
        db_tx.get::<ValidatedPbhTransactionTable>(tx)
    }

    /// Set the store the nullifier for a tx after it
    /// has been included in the block
    /// don't forget to call db_tx.commit() at the very end
    pub fn set_pbh_nullifier(
        &self,
        db_tx: Tx<RW>,
        nullifier: FixedBytes<32>,
    ) -> Result<(), DatabaseError> {
        let mut cursor = db_tx.cursor_write::<ExecutedPbhNullifierTable>()?;
        cursor.insert(nullifier, EmptyValue)?;
        Ok(())
    }
}

/// Implementation of the [`PayloadBuilder`] trait for [`WorldChainPayloadBuilder`].
impl<Pool, Client, EvmConfig> PayloadBuilder<Pool, Client> for WorldChainPayloadBuilder<EvmConfig>
where
    Client: StateProviderFactory,
    Pool: TransactionPool<Transaction: WorldChainPoolTransaction>,
    EvmConfig: ConfigureEvm<Header = Header>,
{
    type Attributes = OptimismPayloadBuilderAttributes;
    type BuiltPayload = OptimismBuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<Pool, Client, OptimismPayloadBuilderAttributes, OptimismBuiltPayload>,
    ) -> Result<BuildOutcome<OptimismBuiltPayload>, PayloadBuilderError> {
        let (cfg_env, block_env) = self
            .inner
            .cfg_and_block_env(&args.config, &args.config.parent_block);

        worldchain_payload(
            self.inner.evm_config.clone(),
            args,
            cfg_env,
            block_env,
            self.verified_blockspace_capacity,
            self.inner.compute_pending_block,
        )
    }

    fn on_missing_payload(
        &self,
        _args: BuildArguments<Pool, Client, OptimismPayloadBuilderAttributes, OptimismBuiltPayload>,
    ) -> MissingPayloadBehaviour<Self::BuiltPayload> {
        MissingPayloadBehaviour::AwaitInProgress
    }

    fn build_empty_payload(
        &self,
        client: &Client,
        config: PayloadConfig<Self::Attributes>,
    ) -> Result<OptimismBuiltPayload, PayloadBuilderError> {
        let args = BuildArguments {
            client,
            config,
            // we use defaults here because for the empty payload we don't need to execute anything
            pool: NoopWorldChainTransactionPool::default(),
            cached_reads: Default::default(),
            cancel: Default::default(),
            best_payload: None,
        };
        let (cfg_env, block_env) = self
            .inner
            .cfg_and_block_env(&args.config, &args.config.parent_block);

        worldchain_payload(
            self.inner.evm_config.clone(),
            args,
            cfg_env,
            block_env,
            self.verified_blockspace_capacity,
            false,
        )?
        .into_payload()
        .ok_or_else(|| PayloadBuilderError::MissingPayload)
    }
}

#[derive(Debug, Default, Clone)]
pub struct WorldChainPayloadServiceBuilder {
    pub verified_blockspace_capacity: u64,
}

impl WorldChainPayloadServiceBuilder {
    pub const fn new(verified_blockspace_capacity: u64) -> Self {
        Self {
            verified_blockspace_capacity,
        }
    }
}

impl<Node, Pool> PayloadServiceBuilder<Node, Pool> for WorldChainPayloadServiceBuilder
where
    Node: FullNodeTypes<
        Types: NodeTypesWithEngine<Engine = OptimismEngineTypes, ChainSpec = ChainSpec>,
    >,
    Pool: TransactionPool<Transaction: WorldChainPoolTransaction> + Unpin + 'static,
{
    async fn spawn_payload_service(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<PayloadBuilderHandle<OptimismEngineTypes>> {
        let data_dir = ctx.config().datadir();
        let db = load_world_chain_db(data_dir.data_dir(), false)?;

        let evm_config = OptimismEvmConfig::new(Arc::new(OpChainSpec {
            inner: (*ctx.chain_spec()).clone(),
        }));

        let payload_builder =
            WorldChainPayloadBuilder::new(evm_config, self.verified_blockspace_capacity, db);

        let conf = ctx.payload_builder_config();

        let payload_job_config = BasicPayloadJobGeneratorConfig::default()
            .interval(conf.interval())
            .deadline(conf.deadline())
            .max_payload_tasks(conf.max_payload_tasks())
            // no extradata for OP
            .extradata(Default::default());

        let payload_generator = BasicPayloadJobGenerator::with_builder(
            ctx.provider().clone(),
            pool,
            ctx.task_executor().clone(),
            payload_job_config,
            ctx.chain_spec(),
            payload_builder,
        );
        let (payload_service, payload_builder) =
            PayloadBuilderService::new(payload_generator, ctx.provider().canonical_state_stream());

        ctx.task_executor()
            .spawn_critical("payload builder service", Box::pin(payload_service));

        Ok(payload_builder)
    }
}

/// Constructs an Ethereum transaction payload from the transactions sent through the
/// Payload attributes by the sequencer. If the `no_tx_pool` argument is passed in
/// the payload attributes, the transaction pool will be ignored and the only transactions
/// included in the payload will be those sent through the attributes.
///
/// Given build arguments including an Ethereum client, transaction pool,
/// and configuration, this function creates a transaction payload. Returns
/// a result indicating success with the payload or an error in case of failure.
#[inline]
pub(crate) fn worldchain_payload<EvmConfig, Pool, Client>(
    evm_config: EvmConfig,
    args: BuildArguments<Pool, Client, OptimismPayloadBuilderAttributes, OptimismBuiltPayload>,
    initialized_cfg: CfgEnvWithHandlerCfg,
    initialized_block_env: BlockEnv,
    verified_blockspace_capacity: u64,
    _compute_pending_block: bool,
) -> Result<BuildOutcome<OptimismBuiltPayload>, PayloadBuilderError>
where
    EvmConfig: ConfigureEvm<Header = Header>,
    Client: StateProviderFactory,
    Pool: TransactionPool<Transaction: WorldChainPoolTransaction>,
{
    let BuildArguments {
        client,
        pool,
        mut cached_reads,
        config,
        cancel,
        best_payload,
    } = args;

    let state_provider = client.state_by_block_hash(config.parent_block.hash())?;
    let state = StateProviderDatabase::new(state_provider);
    let mut db = State::builder()
        .with_database_ref(cached_reads.as_db(state))
        .with_bundle_update()
        .build();
    let PayloadConfig {
        parent_block,
        attributes,
        chain_spec,
        extra_data,
    } = config;

    debug!(target: "payload_builder", id=%attributes.payload_attributes.payload_id(), parent_hash = ?parent_block.hash(), parent_number = parent_block.number, "building new payload");

    let mut cumulative_gas_used = 0;
    let block_gas_limit: u64 = attributes.gas_limit.unwrap_or_else(|| {
        initialized_block_env
            .gas_limit
            .try_into()
            .unwrap_or(chain_spec.max_gas_limit)
    });
    let base_fee = initialized_block_env.basefee.to::<u64>();

    let mut executed_txs = Vec::with_capacity(attributes.transactions.len());
    let mut executed_senders = Vec::with_capacity(attributes.transactions.len());

    let mut best_txs = pool.best_transactions_with_attributes(BestTransactionsAttributes::new(
        base_fee,
        initialized_block_env
            .get_blob_gasprice()
            .map(|gasprice| gasprice as u64),
    ));

    let mut total_fees = U256::ZERO;

    let block_number = initialized_block_env.number.to::<u64>();

    let is_regolith = chain_spec.is_fork_active_at_timestamp(
        OptimismHardfork::Regolith,
        attributes.payload_attributes.timestamp,
    );

    // apply eip-4788 pre block contract call
    pre_block_beacon_root_contract_call(
        &mut db,
        &evm_config,
        &chain_spec,
        &initialized_cfg,
        &initialized_block_env,
        attributes.payload_attributes.parent_beacon_block_root,
    )
    .map_err(|err| {
        warn!(target: "payload_builder",
            parent_hash=%parent_block.hash(),
            %err,
            "failed to apply beacon root contract call for empty payload"
        );
        PayloadBuilderError::Internal(err.into())
    })?;

    // Ensure that the create2deployer is force-deployed at the canyon transition. Optimism
    // blocks will always have at least a single transaction in them (the L1 info transaction),
    // so we can safely assume that this will always be triggered upon the transition and that
    // the above check for empty blocks will never be hit on OP chains.
    reth_evm_optimism::ensure_create2_deployer(
        chain_spec.clone(),
        attributes.payload_attributes.timestamp,
        &mut db,
    )
    .map_err(|err| {
        warn!(target: "payload_builder", %err, "missing create2 deployer, skipping block.");
        PayloadBuilderError::other(OptimismPayloadBuilderError::ForceCreate2DeployerFail)
    })?;

    let mut receipts = Vec::with_capacity(attributes.transactions.len());
    for sequencer_tx in &attributes.transactions {
        // Check if the job was cancelled, if so we can exit early.
        if cancel.is_cancelled() {
            return Ok(BuildOutcome::Cancelled);
        }

        // A sequencer's block should never contain blob transactions.
        if sequencer_tx.value().is_eip4844() {
            return Err(PayloadBuilderError::other(
                OptimismPayloadBuilderError::BlobTransactionRejected,
            ));
        }

        // Convert the transaction to a [TransactionSignedEcRecovered]. This is
        // purely for the purposes of utilizing the `evm_config.tx_env`` function.
        // Deposit transactions do not have signatures, so if the tx is a deposit, this
        // will just pull in its `from` address.
        let sequencer_tx = sequencer_tx
            .value()
            .clone()
            .try_into_ecrecovered()
            .map_err(|_| {
                PayloadBuilderError::other(OptimismPayloadBuilderError::TransactionEcRecoverFailed)
            })?;

        // Cache the depositor account prior to the state transition for the deposit nonce.
        //
        // Note that this *only* needs to be done post-regolith hardfork, as deposit nonces
        // were not introduced in Bedrock. In addition, regular transactions don't have deposit
        // nonces, so we don't need to touch the DB for those.
        let depositor = (is_regolith && sequencer_tx.is_deposit())
            .then(|| {
                db.load_cache_account(sequencer_tx.signer())
                    .map(|acc| acc.account_info().unwrap_or_default())
            })
            .transpose()
            .map_err(|_| {
                PayloadBuilderError::other(OptimismPayloadBuilderError::AccountLoadFailed(
                    sequencer_tx.signer(),
                ))
            })?;

        let env = EnvWithHandlerCfg::new_with_cfg_env(
            initialized_cfg.clone(),
            initialized_block_env.clone(),
            evm_config.tx_env(&sequencer_tx),
        );

        let mut evm = evm_config.evm_with_env(&mut db, env);

        let ResultAndState { result, state } = match evm.transact() {
            Ok(res) => res,
            Err(err) => {
                match err {
                    EVMError::Transaction(err) => {
                        trace!(target: "payload_builder", %err, ?sequencer_tx, "Error in sequencer transaction, skipping.");
                        continue;
                    }
                    err => {
                        // this is an error that we should treat as fatal for this attempt
                        return Err(PayloadBuilderError::EvmExecutionError(err));
                    }
                }
            }
        };

        // to release the db reference drop evm.
        drop(evm);
        // commit changes
        db.commit(state);

        let gas_used = result.gas_used();

        // add gas used by the transaction to cumulative gas used, before creating the receipt
        cumulative_gas_used += gas_used;

        // Push transaction changeset and calculate header bloom filter for receipt.
        receipts.push(Some(Receipt {
            tx_type: sequencer_tx.tx_type(),
            success: result.is_success(),
            cumulative_gas_used,
            logs: result.into_logs().into_iter().map(Into::into).collect(),
            deposit_nonce: depositor.map(|account| account.nonce),
            // The deposit receipt version was introduced in Canyon to indicate an update to how
            // receipt hashes should be computed when set. The state transition process
            // ensures this is only set for post-Canyon deposit transactions.
            deposit_receipt_version: chain_spec
                .is_fork_active_at_timestamp(
                    OptimismHardfork::Canyon,
                    attributes.payload_attributes.timestamp,
                )
                .then_some(1),
        }));

        // append sender and transaction to the respective lists
        executed_senders.push(sequencer_tx.signer());
        executed_txs.push(sequencer_tx.into_signed());
    }

    if !attributes.no_tx_pool {
        let verified_gas_limit = (verified_blockspace_capacity * block_gas_limit) / 100;
        while let Some(pool_tx) = best_txs.next() {
            // If the transaction is verified, check if it can be added within the verified gas limit
            if pool_tx.transaction.semaphore_proof().is_some()
                && cumulative_gas_used + pool_tx.gas_limit() > verified_gas_limit
            {
                best_txs.mark_invalid(&pool_tx);
                continue;
            }

            // ensure we still have capacity for this transaction
            if cumulative_gas_used + pool_tx.gas_limit() > block_gas_limit {
                // we can't fit this transaction into the block, so we need to mark it as
                // invalid which also removes all dependent transaction from
                // the iterator before we can continue
                best_txs.mark_invalid(&pool_tx);
                continue;
            }

            // A sequencer's block should never contain blob or deposit transactions from the pool.
            if pool_tx.is_eip4844() || pool_tx.tx_type() == TxType::Deposit as u8 {
                best_txs.mark_invalid(&pool_tx);
                continue;
            }

            // check if the job was cancelled, if so we can exit early
            if cancel.is_cancelled() {
                return Ok(BuildOutcome::Cancelled);
            }

            // convert tx to a signed transaction
            let tx = pool_tx.to_recovered_transaction();
            let env = EnvWithHandlerCfg::new_with_cfg_env(
                initialized_cfg.clone(),
                initialized_block_env.clone(),
                evm_config.tx_env(&tx),
            );

            // Configure the environment for the block.
            let mut evm = evm_config.evm_with_env(&mut db, env);

            let ResultAndState { result, state } = match evm.transact() {
                Ok(res) => res,
                Err(err) => {
                    match err {
                        EVMError::Transaction(err) => {
                            if matches!(err, InvalidTransaction::NonceTooLow { .. }) {
                                // if the nonce is too low, we can skip this transaction
                                trace!(target: "payload_builder", %err, ?tx, "skipping nonce too low transaction");
                            } else {
                                // if the transaction is invalid, we can skip it and all of its
                                // descendants
                                trace!(target: "payload_builder", %err, ?tx, "skipping invalid transaction and its descendants");
                                best_txs.mark_invalid(&pool_tx);
                            }

                            continue;
                        }
                        err => {
                            // this is an error that we should treat as fatal for this attempt
                            return Err(PayloadBuilderError::EvmExecutionError(err));
                        }
                    }
                }
            };
            // drop evm so db is released.
            drop(evm);
            // commit changes
            db.commit(state);

            let gas_used = result.gas_used();

            // add gas used by the transaction to cumulative gas used, before creating the
            // receipt
            cumulative_gas_used += gas_used;

            // Push transaction changeset and calculate header bloom filter for receipt.
            receipts.push(Some(Receipt {
                tx_type: tx.tx_type(),
                success: result.is_success(),
                cumulative_gas_used,
                logs: result.into_logs().into_iter().map(Into::into).collect(),
                deposit_nonce: None,
                deposit_receipt_version: None,
            }));

            // update add to total fees
            let miner_fee = tx
                .effective_tip_per_gas(Some(base_fee))
                .expect("fee is always valid; execution succeeded");
            total_fees += U256::from(miner_fee) * U256::from(gas_used);

            // append sender and transaction to the respective lists
            executed_senders.push(tx.signer());
            executed_txs.push(tx.into_signed());
        }
    }

    // check if we have a better block
    if !is_better_payload(best_payload.as_ref(), total_fees) {
        // can skip building the block
        return Ok(BuildOutcome::Aborted {
            fees: total_fees,
            cached_reads,
        });
    }

    let WithdrawalsOutcome {
        withdrawals_root,
        withdrawals,
    } = commit_withdrawals(
        &mut db,
        &chain_spec,
        attributes.payload_attributes.timestamp,
        attributes.clone().payload_attributes.withdrawals,
    )?;

    // merge all transitions into bundle state, this would apply the withdrawal balance changes
    // and 4788 contract call
    db.merge_transitions(BundleRetention::Reverts);

    let execution_outcome = ExecutionOutcome::new(
        db.take_bundle(),
        vec![receipts.clone()].into(),
        block_number,
        Vec::new(),
    );
    let receipts_root = execution_outcome
        .optimism_receipts_root_slow(
            block_number,
            chain_spec.as_ref(),
            attributes.payload_attributes.timestamp,
        )
        .expect("Number is in range");
    let logs_bloom = execution_outcome
        .block_logs_bloom(block_number)
        .expect("Number is in range");

    // calculate the state root
    let hashed_state = HashedPostState::from_bundle_state(&execution_outcome.state().state);
    let (state_root, trie_output) = {
        let state_provider = db.database.0.inner.borrow_mut();
        state_provider
            .db
            .state_root_with_updates(hashed_state.clone())
            .inspect_err(|err| {
                warn!(target: "payload_builder",
                    parent_hash=%parent_block.hash(),
                    %err,
                    "failed to calculate state root for empty payload"
                );
            })?
    };

    // create the block header
    let transactions_root = proofs::calculate_transaction_root(&executed_txs);

    // initialize empty blob sidecars. There are no blob transactions on L2.
    let blob_sidecars = Vec::new();
    let mut excess_blob_gas = None;
    let mut blob_gas_used = None;

    // only determine cancun fields when active
    if chain_spec.is_cancun_active_at_timestamp(attributes.payload_attributes.timestamp) {
        excess_blob_gas = if chain_spec.is_cancun_active_at_timestamp(parent_block.timestamp) {
            let parent_excess_blob_gas = parent_block.excess_blob_gas.unwrap_or_default();
            let parent_blob_gas_used = parent_block.blob_gas_used.unwrap_or_default();
            Some(calculate_excess_blob_gas(
                parent_excess_blob_gas,
                parent_blob_gas_used,
            ))
        } else {
            // for the first post-fork block, both parent.blob_gas_used and
            // parent.excess_blob_gas are evaluated as 0
            Some(calculate_excess_blob_gas(0, 0))
        };

        blob_gas_used = Some(0);
    }

    let header = Header {
        parent_hash: parent_block.hash(),
        ommers_hash: EMPTY_OMMER_ROOT_HASH,
        beneficiary: initialized_block_env.coinbase,
        state_root,
        transactions_root,
        receipts_root,
        withdrawals_root,
        logs_bloom,
        timestamp: attributes.payload_attributes.timestamp,
        mix_hash: attributes.payload_attributes.prev_randao,
        nonce: BEACON_NONCE,
        base_fee_per_gas: Some(base_fee),
        number: parent_block.number + 1,
        gas_limit: block_gas_limit,
        difficulty: U256::ZERO,
        gas_used: cumulative_gas_used,
        extra_data,
        parent_beacon_block_root: attributes.payload_attributes.parent_beacon_block_root,
        blob_gas_used,
        excess_blob_gas,
        requests_root: None,
    };

    // seal the block
    let block = Block {
        header,
        body: executed_txs,
        ommers: vec![],
        withdrawals,
        requests: None,
    };

    let sealed_block = block.seal_slow();
    debug!(target: "payload_builder", ?sealed_block, "sealed built block");

    // create the executed block data
    let executed = ExecutedBlock {
        block: Arc::new(sealed_block.clone()),
        senders: Arc::new(executed_senders),
        execution_output: Arc::new(execution_outcome),
        hashed_state: Arc::new(hashed_state),
        trie: Arc::new(trie_output),
    };

    let mut payload = OptimismBuiltPayload::new(
        attributes.payload_attributes.id,
        sealed_block,
        total_fees,
        chain_spec,
        attributes,
        Some(executed),
    );

    // extend the payload with the blob sidecars from the executed txs
    payload.extend_sidecars(blob_sidecars);

    Ok(BuildOutcome::Better {
        payload,
        cached_reads,
    })
}

#[cfg(test)]
mod tests {
    use crate::{
        pbh::semaphore::SemaphoreProof,
        pool::{
            ordering::WorldChainOrdering, tx::WorldChainPooledTransaction,
            validator::WorldChainTransactionValidator,
        },
    };

    use super::*;
    use alloy_consensus::TxLegacy;
    use futures::future::join_all;
    use reth_db::test_utils::tempdir_path;
    use reth_evm_optimism::OptimismEvmConfig;
    use reth_node_optimism::txpool::OpTransactionValidator;
    use reth_optimism_chainspec::OpChainSpec;
    use reth_payload_builder::{EthPayloadBuilderAttributes, PayloadId};
    use reth_primitives::{
        SealedBlock, Signature, TransactionSigned, TransactionSignedEcRecovered, Withdrawals,
    };
    use reth_provider::{test_utils::NoopProvider, BlockReaderIdExt};
    use reth_transaction_pool::{
        blobstore::DiskFileBlobStore,
        validate::{EthTransactionValidatorBuilder, ValidTransaction},
        EthPooledTransaction, PoolConfig, PoolTransaction, TransactionOrigin,
        TransactionValidationOutcome, TransactionValidator,
    };
    use revm_primitives::{Address, Bytes, B256};
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
        let client = NoopProvider::default();
        let eth_tx_validator = EthTransactionValidatorBuilder::new(chain_spec.clone())
            .build(client, blob_store.clone());
        let op_tx_validator =
            OpTransactionValidator::new(eth_tx_validator).require_l1_data_gas_fee(false);

        let wc_validator = WorldChainTransactionValidator::new(op_tx_validator, db.clone(), 30);

        let wc_noop_validator = WorldChainNoopValidator::new(wc_validator);
        let ordering = WorldChainOrdering::new(db.clone());

        let world_chain_tx_pool = reth_transaction_pool::Pool::new(
            wc_noop_validator,
            ordering,
            blob_store,
            PoolConfig::default(),
        );

        // Init the payload builder
        let verified_blockspace_cap = 50;
        let world_chain_payload_builder =
            WorldChainPayloadBuilder::new(evm_config, verified_blockspace_cap, db.clone());

        // Insert transactions into the pool
        let unverified_transactions = generate_mock_world_chain_transactions(50, 100000, false);
        for transaction in unverified_transactions.iter() {
            world_chain_tx_pool
                .add_transaction(TransactionOrigin::Local, transaction.clone())
                .await?;
        }

        // Insert verifiedtransactions into the pool
        let verified_transactions = generate_mock_world_chain_transactions(50, 100000, true);
        for transaction in verified_transactions.iter() {
            world_chain_tx_pool
                .add_transaction(TransactionOrigin::Local, transaction.clone())
                .await?;
        }

        let sequencer_transactions = (0..50)
            .map(|_| generate_mock_signed_transaction(10000))
            .collect::<Vec<_>>();

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
            // TODO: add sequencer txs
            transactions: vec![],
            payload_attributes: eth_payload_attributes,
            no_tx_pool: false,
        };

        let build_args = BuildArguments {
            client: NoopProvider::default(),
            config: PayloadConfig {
                parent_block: Arc::new(SealedBlock::default()),
                attributes: payload_attributes,
                chain_spec: chain_spec,
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
            .map(|tx| tx.hash())
            .collect::<Vec<_>>();
        expected_order.extend(verified_transactions.iter().map(|tx| tx.hash()));
        expected_order.extend(unverified_transactions.iter().map(|tx| tx.hash()));

        for (tx, expected_hash) in built_payload.block().body.iter().zip(expected_order.iter()) {
            assert_eq!(tx.hash, *expected_hash);
        }

        Ok(())
    }

    pub struct WorldChainNoopValidator<Client, Tx>
    where
        Client: StateProviderFactory + BlockReaderIdExt,
    {
        inner: WorldChainTransactionValidator<Client, Tx>,
    }

    impl WorldChainNoopValidator<NoopProvider, WorldChainPooledTransaction> {
        pub fn new(
            inner: WorldChainTransactionValidator<NoopProvider, WorldChainPooledTransaction>,
        ) -> Self {
            Self { inner }
        }
    }

    impl<Client, Tx> TransactionValidator for WorldChainNoopValidator<Client, Tx>
    where
        Client: StateProviderFactory + BlockReaderIdExt,
        Tx: WorldChainPoolTransaction,
    {
        type Transaction = Tx;

        async fn validate_transaction(
            &self,
            _origin: TransactionOrigin,
            transaction: Self::Transaction,
        ) -> TransactionValidationOutcome<Self::Transaction> {
            if let Some(semaphore_proof) = transaction.semaphore_proof() {
                self.inner
                    .set_validated(&transaction, semaphore_proof)
                    .expect("Error when writing to the db");
            }

            TransactionValidationOutcome::Valid {
                balance: U256::ZERO,
                state_nonce: 0,
                transaction: ValidTransaction::Valid(transaction),
                propagate: false,
            }
        }

        async fn validate_transactions(
            &self,
            transactions: Vec<(TransactionOrigin, Self::Transaction)>,
        ) -> Vec<TransactionValidationOutcome<Self::Transaction>> {
            let futures = transactions
                .into_iter()
                .map(|(origin, tx)| self.validate_transaction(origin, tx))
                .collect::<Vec<_>>();

            join_all(futures).await
        }

        fn on_new_head_block(&self, _new_tip_block: &SealedBlock) {
            unreachable!("NoopValidator does not implement on_new_head_block");
        }
    }

    fn generate_mock_signed_transaction(gas_limit: u64) -> TransactionSigned {
        let tx = reth_primitives::Transaction::Legacy(TxLegacy {
            gas_price: 10,
            gas_limit: gas_limit.into(),
            value: U256::from(100),
            ..Default::default()
        });
        let signature = Signature::default();
        TransactionSigned::from_transaction_and_signature(tx, signature)
    }

    fn generate_mock_world_chain_transactions(
        count: usize,
        gas_limit: u64,
        pbh: bool,
    ) -> Vec<WorldChainPooledTransaction> {
        let signed_txs = (0..count)
            .map(|_| generate_mock_signed_transaction(gas_limit))
            .collect::<Vec<_>>();

        signed_txs
            .iter()
            .map(|tx| {
                let transaction = TransactionSignedEcRecovered::from_signed_transaction(
                    tx.clone(),
                    Default::default(),
                );
                let pooled_tx = EthPooledTransaction::new(transaction.clone(), 200);

                let semaphore_proof = if pbh {
                    Some(SemaphoreProof::default())
                } else {
                    None
                };

                WorldChainPooledTransaction {
                    inner: pooled_tx,
                    semaphore_proof,
                }
            })
            .collect()
    }
}
