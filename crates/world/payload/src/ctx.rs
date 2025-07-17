use alloy_consensus::{SignableTransaction, Transaction};
use alloy_eips::Typed2718;
use alloy_network::{TransactionBuilder, TxSignerSync};
use alloy_op_evm::block::receipt_builder::OpReceiptBuilder;
use alloy_op_evm::OpEvm;
use alloy_rlp::Encodable;
use alloy_signer::k256::elliptic_curve::Error;
use alloy_signer_local::PrivateKeySigner;
use eyre::eyre::eyre;
use flashblocks::builder::ExecutionInfo;
use flashblocks::payload_builder_ctx::{PayloadBuilderCtx, PayloadBuilderCtxBuilder};
use op_alloy_consensus::OpTransaction;
use op_alloy_rpc_types::OpTransactionRequest;
use reth::api::PayloadBuilderError;
use reth::core::rpc::result;
use reth::payload::{PayloadBuilderAttributes, PayloadId};
use reth::revm::cancelled::CancelOnDrop;
use reth::revm::State;
use reth::transaction_pool::{BestTransactionsAttributes, TransactionPool};
use reth_basic_payload_builder::PayloadConfig;
use reth_evm::eth::receipt_builder::ReceiptBuilderCtx;
use reth_evm::execute::{BlockBuilder, BlockExecutor};
use reth_evm::op_revm::{OpHaltReason, OpSpecId, OpTransactionError};
use reth_evm::precompiles::PrecompilesMap;
use reth_evm::{ConfigureEvm, Database};
use reth_evm::{Evm, EvmError};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::txpool::estimated_da_size::DataAvailabilitySized;
use reth_optimism_node::{
    OpBuiltPayload, OpEvmConfig, OpNextBlockEnvAttributes, OpPayloadBuilderAttributes,
    OpRethReceiptBuilder,
};
use reth_optimism_payload_builder::builder::OpPayloadBuilderCtx;
use reth_optimism_payload_builder::config::OpDAConfig;
use reth_optimism_payload_builder::error::OpPayloadBuilderError;
use reth_optimism_primitives::{OpPrimitives, OpReceipt, OpTransactionSigned};
use reth_payload_util::PayloadTransactions;
use reth_primitives::{Block, NodePrimitives, Recovered, SealedHeader, TxTy};
use reth_primitives_traits::{SignedTransaction, SignerRecoverable};
use reth_provider::{BlockReaderIdExt, ChainSpecProvider, StateProviderFactory};
use reth_transaction_pool::PoolTransaction;
use revm::context::result::{
    EVMError, ExecResultAndState, ExecutionResult, HaltReason, ResultAndState,
};
use revm::inspector::NoOpInspector;
use revm::state::Account;
use revm::DatabaseCommit;
use revm_primitives::{Address, HashMap, U256};
use semaphore_rs::Field;
use std::collections::HashSet;
use std::fmt::Debug;
use std::sync::Arc;
use tracing::info;
use tracing::{error, trace};

use world_chain_builder_pool::bindings::IPBHEntryPoint::spendNullifierHashesCall;
use world_chain_builder_pool::tx::{WorldChainPoolTransaction, WorldChainPooledTransaction};
use world_chain_builder_rpc::transactions::validate_conditional_options;

/// Container type that holds all necessities to build a new payload.
#[derive(Debug, Clone)]
pub struct WorldChainPayloadBuilderCtx<Client, Pool>
where
    Client: StateProviderFactory
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
        + ChainSpecProvider<ChainSpec: OpHardforks>
        + Clone,
{
    pub inner: Arc<OpPayloadBuilderCtx<OpEvmConfig, <Client as ChainSpecProvider>::ChainSpec>>,
    pub verified_blockspace_capacity: u8,
    pub pbh_entry_point: Address,
    pub pbh_signature_aggregator: Address,
    pub client: Client,
    pub builder_private_key: PrivateKeySigner,
    pub pool: Pool,
}

#[derive(Debug, Clone)]
pub struct WorldChainPayloadBuilderCtxBuilder<Client, Pool> {
    pub client: Client,
    pub pool: Pool,
    pub verified_blockspace_capacity: u8,
    pub pbh_entry_point: Address,
    pub pbh_signature_aggregator: Address,
    pub builder_private_key: PrivateKeySigner,
}

impl<Client, Pool> WorldChainPayloadBuilderCtx<Client, Pool>
where
    Client: StateProviderFactory
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
        + ChainSpecProvider<ChainSpec: OpHardforks>
        + Clone,
    Pool: Send + Sync + TransactionPool,
{
    /// After computing the execution result and state we can commit changes to the database
    fn commit_changes<DB: Database, E: Default + Debug>(
        &self,
        info: &mut ExecutionInfo<E>,
        base_fee: u64,
        gas_used: u64,
        tx: Recovered<OpTransactionSigned>,
        result: ExecutionResult<OpHaltReason>,
        state: HashMap<Address, Account>,
        evm: &alloy_op_evm::OpEvm<DB, NoOpInspector, PrecompilesMap>,
    ) -> Result<(), PayloadBuilderError> {
        info.cumulative_gas_used += gas_used;
        info.cumulative_da_bytes_used += tx.length() as u64;

        let receipt = OpRethReceiptBuilder::default()
            .build_receipt(ReceiptBuilderCtx {
                tx: &tx,
                evm: evm,
                result: result,
                state: &state,
                cumulative_gas_used: info.cumulative_gas_used,
            })
            .map_err(|_| {
                error!(target: "payload_builder", "failed to build receipt");
                PayloadBuilderError::Other(eyre!("failed to build receipt").into())
            })?;

        // update add to total fees
        let miner_fee = tx
            .effective_tip_per_gas(base_fee)
            .expect("fee is always valid; execution succeeded");
        info.total_fees += U256::from(miner_fee) * U256::from(gas_used);

        info.receipts.push(receipt);

        Ok(())
    }
}

impl<Client, Pool> PayloadBuilderCtx for WorldChainPayloadBuilderCtx<Client, Pool>
where
    Client: StateProviderFactory
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
        + ChainSpecProvider<ChainSpec: OpHardforks>
        + Clone,
    Pool: Send + Sync + TransactionPool,
{
    type Evm = OpEvmConfig;
    type ChainSpec = <Client as ChainSpecProvider>::ChainSpec;
    type Transaction = WorldChainPooledTransaction;

    fn evm_config(&self) -> &Self::Evm {
        &self.inner.evm_config
    }

    fn spec(&self) -> &Self::ChainSpec {
        // TODO: Replace this is `self.inner.spec()` once PayloadBuilderCtx is implemented for
        // inner
        self.inner.chain_spec.as_ref()
    }

    fn evm_env(&self) -> reth_evm::EvmEnv<OpSpecId> {
        self.inner.evm_config.evm_env(self.parent())
    }

    fn parent(&self) -> &SealedHeader {
        self.inner.parent()
    }

    fn attributes(
        &self,
    ) -> &OpPayloadBuilderAttributes<TxTy<<Self::Evm as ConfigureEvm>::Primitives>> {
        self.inner.attributes()
    }

    fn best_transaction_attributes(
        &self,
        block_env: &revm::context::BlockEnv,
    ) -> BestTransactionsAttributes {
        self.inner.best_transaction_attributes(block_env)
    }

    fn payload_id(&self) -> PayloadId {
        self.inner.payload_id()
    }

    fn is_better_payload(&self, total_fees: U256) -> bool {
        self.inner.is_better_payload(total_fees)
    }

    fn block_builder<'a, DB>(
        &'a self,
        db: &'a mut State<DB>,
    ) -> Result<
        impl BlockBuilder<
            Primitives = OpPrimitives,
            Executor: BlockExecutor<Evm: Evm<DB = &'a mut State<DB>>>,
        >,
        PayloadBuilderError,
    >
    where
        DB: revm::Database + std::fmt::Debug + 'a,
        DB::Error: Send + Sync + std::fmt::Debug + 'static,
    {
        // Prepare attributes for next block environment.
        let attributes = OpNextBlockEnvAttributes {
            timestamp: self.inner.attributes().timestamp(),
            suggested_fee_recipient: self.inner.attributes().suggested_fee_recipient(),
            prev_randao: self.inner.attributes().prev_randao(),
            gas_limit: self
                .inner
                .attributes()
                .gas_limit
                .unwrap_or(self.inner.parent().gas_limit),
            parent_beacon_block_root: self.inner.attributes().parent_beacon_block_root(),
            extra_data: self.inner.extra_data()?,
        };

        // Prepare EVM environment.
        let evm_env = self
            .inner
            .evm_config
            .next_evm_env(self.inner.parent(), &attributes)
            .map_err(PayloadBuilderError::other)?;

        // Prepare EVM.
        let evm = self.inner.evm_config.evm_with_env(db, evm_env);

        // Prepare block execution context.
        let execution_ctx = self
            .inner
            .evm_config
            .context_for_next_block(self.inner.parent(), attributes);

        // Prepare block builder.
        Ok(self
            .inner
            .evm_config
            .create_block_builder(evm, self.inner.parent(), execution_ctx))
    }

    fn execute_sequencer_transactions<DB, E: Default + Debug>(
        &self,
        db: &mut Builder,
    ) -> Result<ExecutionInfo<E>, PayloadBuilderError>
    where
        Builder: BlockBuilder<
            Primitives = OpPrimitives,
            Executor: BlockExecutor<Evm = OpEvm<DB, NoOpInspector, PrecompilesMap>>,
        >,
        DB: Database<Error = ProviderError> + 'static,
    {
        self.inner.execute_sequencer_transactions(db)
    }

    /// Executes the given best transactions and updates the execution info.
    ///
    /// Returns `Ok(Some(())` if the job was cancelled.
    fn execute_best_transactions<DB, Builder, Txs, E: Default + Debug>(
        &self,
        info: &mut ExecutionInfo<E>,
        builder: &mut Builder,
        mut best_txs: Txs,
        _gas_limit: u64,
    ) -> Result<Option<()>, PayloadBuilderError>
    where
        Txs: PayloadTransactions<Transaction = Self::Transaction>,
        Builder: BlockBuilder<
            Primitives = OpPrimitives,
            Executor: BlockExecutor<Evm = OpEvm<DB, NoOpInspector, PrecompilesMap>>,
        >,
        DB: Database<Error = ProviderError> + 'static,
    {
        let mut block_gas_limit = evm.block().gas_limit;
        let block_da_limit = self.inner.da_config.max_da_block_size();
        let tx_da_limit = self.inner.da_config.max_da_tx_size();
        let base_fee = evm.block().basefee;

        let mut invalid_txs = vec![];
        let verified_gas_limit = (self.verified_blockspace_capacity as u64 * block_gas_limit) / 100;

        let mut spent_nullifier_hashes = HashSet::new();
        while let Some(pooled_tx) = best_txs.next(()) {
            info!(target: "payload_builder", "Processing transaction: {}", pooled_tx.hash());
            let tx_da_size = pooled_tx.estimated_da_size();
            let tx = pooled_tx.clone().into_consensus();

            if info.is_tx_over_limits(
                tx_da_size,
                block_gas_limit,
                tx_da_limit,
                block_da_limit,
                tx.gas_limit(),
            ) {
                // we can't fit this transaction into the block, so we need to mark it as
                // invalid which also removes all dependent transaction from
                // the iterator before we can continue
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            if let Some(conditional_options) = pooled_tx.conditional_options() {
                if validate_conditional_options(conditional_options, &self.client).is_err() {
                    best_txs.mark_invalid(tx.signer(), tx.nonce());
                    invalid_txs.push(*pooled_tx.hash());
                    continue;
                }
            }

            // A sequencer's block should never contain blob or deposit transactions from the pool.
            if tx.is_eip4844() || tx.is_deposit() {
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            // check if the job was cancelled, if so we can exit early
            if self.inner.cancel.is_cancelled() {
                return Ok(Some(()));
            }

            // If the transaction is verified, check if it can be added within the verified gas limit
            if let Some(payloads) = pooled_tx.pbh_payload() {
                if info.cumulative_gas_used + tx.gas_limit() > verified_gas_limit {
                    best_txs.mark_invalid(tx.signer(), tx.nonce());
                    continue;
                }

                if payloads
                    .iter()
                    .any(|payload| !spent_nullifier_hashes.insert(payload.nullifier_hash))
                {
                    best_txs.mark_invalid(tx.signer(), tx.nonce());
                    invalid_txs.push(*pooled_tx.hash());
                    continue;
                }
            }

            let gas_used = match evm.transact(tx.clone()) {
                Ok(res) => {
                    let gas_used = res.result.gas_used();
                    if let Some(payloads) = pooled_tx.pbh_payload() {
                        if spent_nullifier_hashes.len() == payloads.len() {
                            block_gas_limit -= FIXED_GAS
                        }

                        block_gas_limit -= COLD_SSTORE_GAS * payloads.len() as u64;
                    }

                    let ExecResultAndState { result, state } = res;

                    self.commit_changes(info, base_fee, gas_used, tx, result, state, &evm);

                    gas_used
                }
                Err(err) => {
                    match err {
                        EVMError::Transaction(OpTransactionError::Base(ref _error)) => {
                            // if the transaction is invalid, we can skip it and all of its
                            // descendants
                            // trace!(target: "payload_builder", %err, ?tx, "skipping invalid transaction and its descendants");
                            // best_txs.mark_invalid(tx.signer(), tx.nonce());

                            continue;
                        }

                        err => {
                            // this is an error that we should treat as fatal for this attempt
                            return Err(PayloadBuilderError::EvmExecutionError(Box::new(err)));
                        }
                    }
                }
            };
        }

        if !spent_nullifier_hashes.is_empty() {
            let tx = spend_nullifiers_tx(self, &mut evm, spent_nullifier_hashes).map_err(
                |e| {
                    error!(target: "payload_builder", %e, "failed to build spend nullifiers transaction");
                    PayloadBuilderError::Other(e.into())
                },
            )?;

            // Try to execute the builder tx. In the event that execution fails due to
            // insufficient funds, continue with the built payload. This ensures that
            // PBH transactions still receive priority inclusion, even if the PBH nullifier
            // is not spent rather than sitting in the default execution client's mempool.
            match evm.transact(tx.clone()) {
                Ok(ExecResultAndState { result, state }) => {
                    self.commit_changes(info, base_fee, result.gas_used(), tx, result, state, &evm);
                }

                Err(e) => {
                    error!(target: "payload_builder", %e, "spend nullifiers transaction failed")
                }
            }
        }

        if !invalid_txs.is_empty() {
            self.pool.remove_transactions(invalid_txs);
        }

        Ok(None)
    }
}

impl<Client, Pool> PayloadBuilderCtxBuilder<OpChainSpec, WorldChainPooledTransaction>
    for WorldChainPayloadBuilderCtxBuilder<Client, Pool>
where
    Client: StateProviderFactory
        + ChainSpecProvider<ChainSpec = OpChainSpec>
        + Send
        + Sync
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
        + Clone,
    Pool: Send + Sync + TransactionPool,
{
    type PayloadBuilderCtx = WorldChainPayloadBuilderCtx<Client, Pool>;

    fn build<Txs>(
        &self,
        evm_config: OpEvmConfig,
        da_config: OpDAConfig,
        chain_spec: Arc<OpChainSpec>,
        config: PayloadConfig<
            OpPayloadBuilderAttributes<
                <<OpEvmConfig as ConfigureEvm>::Primitives as NodePrimitives>::SignedTx,
            >,
            <<OpEvmConfig as ConfigureEvm>::Primitives as NodePrimitives>::BlockHeader,
        >,
        cancel: &CancelOnDrop,
        best_payload: Option<OpBuiltPayload<<OpEvmConfig as ConfigureEvm>::Primitives>>,
    ) -> Self::PayloadBuilderCtx
    where
        Self: Sized,
    {
        let inner = OpPayloadBuilderCtx {
            evm_config,
            da_config,
            chain_spec,
            config,
            cancel: cancel.clone(),
            best_payload,
        };

        WorldChainPayloadBuilderCtx {
            inner: Arc::new(inner),
            client: self.client.clone(),
            pool: self.pool.clone(),
            verified_blockspace_capacity: self.verified_blockspace_capacity,
            pbh_entry_point: self.pbh_entry_point,
            pbh_signature_aggregator: self.pbh_signature_aggregator,
            builder_private_key: self.builder_private_key.clone(),
        }
    }
}

pub const COLD_SSTORE_GAS: u64 = 20000;
pub const FIXED_GAS: u64 = 100_000;

pub const fn dyn_gas_limit(len: u64) -> u64 {
    FIXED_GAS + len * COLD_SSTORE_GAS
}

pub fn spend_nullifiers_tx<DB, EVM, Client, Pool>(
    ctx: &WorldChainPayloadBuilderCtx<Client, Pool>,
    evm: &mut EVM,
    nullifier_hashes: HashSet<Field>,
) -> eyre::Result<Recovered<OpTransactionSigned>>
where
    Client: StateProviderFactory
        + ChainSpecProvider<ChainSpec: OpHardforks>
        + Send
        + Sync
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
        + Clone,
    Pool: Send + Sync + TransactionPool,
    EVM: Evm<DB = DB>,
    DB: revm::Database,
    <DB as revm::Database>::Error: Send + Sync + 'static,
{
    let nonce = evm
        .db_mut()
        .basic(ctx.builder_private_key.address())?
        .unwrap_or_default()
        .nonce;

    let mut tx = OpTransactionRequest::default()
        .nonce(nonce)
        .gas_limit(dyn_gas_limit(nullifier_hashes.len() as u64))
        .max_priority_fee_per_gas(evm.block().basefee.into())
        .max_fee_per_gas(evm.block().basefee.into())
        .with_chain_id(evm.chain_id())
        .with_call(&spendNullifierHashesCall {
            _nullifierHashes: nullifier_hashes.into_iter().collect(),
        })
        .to(ctx.pbh_entry_point)
        .build_typed_tx()
        .map_err(|e| eyre!("{:?}", e))?;

    let signature = ctx.builder_private_key.sign_transaction_sync(&mut tx)?;
    let signed: OpTransactionSigned = tx.into_signed(signature).into();
    Ok(signed.try_into_recovered_unchecked()?)
}
