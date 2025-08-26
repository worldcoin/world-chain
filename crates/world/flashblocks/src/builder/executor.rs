use alloy_eips::eip2718::WithEncoded;
use eyre::eyre::bail;
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use futures::StreamExt as _;
use reth_node_builder::BuilderContext;
use reth_payload_util::BestPayloadTransactions;
use rollup_boost::{AuthorizedMsg, AuthorizedPayload, FlashblocksPayloadV1};
use std::borrow::Cow;
use std::sync::Arc;
use tracing::error;
use world_chain_provider::InMemoryState;

use alloy_consensus::{
    Block, Eip658Value, Header, Transaction, TxReceipt, EMPTY_OMMER_ROOT_HASH, EMPTY_ROOT_HASH,
};
use alloy_eips::eip4895::Withdrawals;
use alloy_eips::{Decodable2718, Encodable2718, Typed2718};
use alloy_op_evm::block::receipt_builder::OpReceiptBuilder;
use alloy_op_evm::{OpBlockExecutionCtx, OpBlockExecutorFactory, OpEvmFactory};
use alloy_primitives::{address, b256, hex, Address, Bytes, B256, B64};
use op_alloy_consensus::{OpDepositReceipt, OpTxEnvelope};
use parking_lot::RwLock;
use reth::core::primitives::Receipt;
use reth::payload::EthPayloadBuilderAttributes;
use reth::revm::cancelled::CancelOnDrop;
use reth::revm::State;
use reth_basic_payload_builder::{BuildOutcomeKind, PayloadConfig};
use reth_evm::block::{
    BlockExecutorFactory, BlockExecutorFor, BlockValidationError, StateChangePostBlockSource,
    StateChangeSource, SystemCaller,
};
use reth_evm::eth::receipt_builder::ReceiptBuilderCtx;
use reth_evm::execute::{
    BasicBlockBuilder, BlockAssembler, BlockAssemblerInput, BlockBuilder, BlockBuilderOutcome,
    ExecutorTx,
};
use reth_evm::op_revm::transaction::deposit::DEPOSIT_TRANSACTION_TYPE;
use reth_evm::op_revm::{OpHaltReason, OpSpecId};
use reth_evm::state_change::{balance_increment_state, post_block_balance_increments};
use reth_evm::{
    block::{BlockExecutionError, BlockExecutor, CommitChanges, ExecutableTx},
    Database, FromRecoveredTx, FromTxWithEncoded, OnStateHook,
};
use reth_evm::{Evm, EvmFactory};
use reth_node_api::{BuiltPayload as _, FullNodeTypes, NodeTypes};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::txpool::OpPooledTx;
use reth_optimism_node::{
    OpBlockAssembler, OpBuiltPayload, OpDAConfig, OpEvmConfig, OpPayloadBuilderAttributes,
    OpRethReceiptBuilder,
};
use reth_optimism_primitives::{DepositReceipt, OpPrimitives, OpReceipt, OpTransactionSigned};
use reth_primitives::{transaction::SignedTransaction, SealedHeader};
use reth_primitives::{NodePrimitives, Recovered};
use reth_provider::{BlockExecutionResult, StateProvider, StateProviderFactory as _};
use revm::context::result::{ExecutionResult, ResultAndState};
use revm::database::BundleState;
use revm::primitives::HashMap;
use revm::state::Bytecode;
use revm::DatabaseCommit;

use crate::primitives::{Flashblock, Flashblocks};
use crate::{FlashblockBuilder, PayloadBuilderCtxBuilder};

/// The address of the create2 deployer
const CREATE_2_DEPLOYER_ADDR: Address = address!("0x13b0D85CcB8bf860b6b79AF3029fCA081AE9beF2");

/// The codehash of the create2 deployer contract.
const CREATE_2_DEPLOYER_CODEHASH: B256 =
    b256!("0xb0550b5b431e30d38000efb7107aaa0ade03d48a7198a140edda9d27134468b2");

/// The raw bytecode of the create2 deployer contract.
const CREATE_2_DEPLOYER_BYTECODE: [u8; 1584] = hex!("6080604052600436106100435760003560e01c8063076c37b21461004f578063481286e61461007157806356299481146100ba57806366cfa057146100da57600080fd5b3661004a57005b600080fd5b34801561005b57600080fd5b5061006f61006a366004610327565b6100fa565b005b34801561007d57600080fd5b5061009161008c366004610327565b61014a565b60405173ffffffffffffffffffffffffffffffffffffffff909116815260200160405180910390f35b3480156100c657600080fd5b506100916100d5366004610349565b61015d565b3480156100e657600080fd5b5061006f6100f53660046103ca565b610172565b61014582826040518060200161010f9061031a565b7fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe082820381018352601f90910116604052610183565b505050565b600061015683836102e7565b9392505050565b600061016a8484846102f0565b949350505050565b61017d838383610183565b50505050565b6000834710156101f4576040517f08c379a000000000000000000000000000000000000000000000000000000000815260206004820152601d60248201527f437265617465323a20696e73756666696369656e742062616c616e636500000060448201526064015b60405180910390fd5b815160000361025f576040517f08c379a000000000000000000000000000000000000000000000000000000000815260206004820181905260248201527f437265617465323a2062797465636f6465206c656e677468206973207a65726f60448201526064016101eb565b8282516020840186f5905073ffffffffffffffffffffffffffffffffffffffff8116610156576040517f08c379a000000000000000000000000000000000000000000000000000000000815260206004820152601960248201527f437265617465323a204661696c6564206f6e206465706c6f790000000000000060448201526064016101eb565b60006101568383305b6000604051836040820152846020820152828152600b8101905060ff815360559020949350505050565b61014e806104ad83390190565b6000806040838503121561033a57600080fd5b50508035926020909101359150565b60008060006060848603121561035e57600080fd5b8335925060208401359150604084013573ffffffffffffffffffffffffffffffffffffffff8116811461039057600080fd5b809150509250925092565b7f4e487b7100000000000000000000000000000000000000000000000000000000600052604160045260246000fd5b6000806000606084860312156103df57600080fd5b8335925060208401359150604084013567ffffffffffffffff8082111561040557600080fd5b818601915086601f83011261041957600080fd5b81358181111561042b5761042b61039b565b604051601f82017fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe0908116603f011681019083821181831017156104715761047161039b565b8160405282815289602084870101111561048a57600080fd5b826020860160208301376000602084830101528095505050505050925092509256fe608060405234801561001057600080fd5b5061012e806100206000396000f3fe6080604052348015600f57600080fd5b506004361060285760003560e01c8063249cb3fa14602d575b600080fd5b603c603836600460b1565b604e565b60405190815260200160405180910390f35b60008281526020818152604080832073ffffffffffffffffffffffffffffffffffffffff8516845290915281205460ff16608857600060aa565b7fa2ef4600d742022d532d4747cb3547474667d6f13804902513b2ec01c848f4b45b9392505050565b6000806040838503121560c357600080fd5b82359150602083013573ffffffffffffffffffffffffffffffffffffffff8116811460ed57600080fd5b80915050925092905056fea26469706673582212205ffd4e6cede7d06a5daf93d48d0541fc68189eeb16608c1999a82063b666eb1164736f6c63430008130033a2646970667358221220fdc4a0fe96e3b21c108ca155438d37c9143fb01278a3c1d274948bad89c564ba64736f6c63430008130033");

/// A Block Executor for Optimism that can load pre state from previous flashblocks.
#[derive(Debug)]
pub struct FlashblocksBlockExecutor<Evm, R: OpReceiptBuilder, Spec> {
    /// Spec.
    spec: Spec,
    /// Receipt builder.
    receipt_builder: R,

    /// Context for block execution.
    ctx: OpBlockExecutionCtx,
    /// The EVM used by executor.
    evm: Evm,
    /// Receipts of executed transactions.
    receipts: Vec<R::Receipt>,
    /// Total gas used by executed transactions.
    gas_used: u64,
    /// Whether Regolith hardfork is active.
    is_regolith: bool,
    /// Utility to call system smart contracts.
    system_caller: SystemCaller<Spec>,
}

impl<'db, DB, E, R, Spec> FlashblocksBlockExecutor<E, R, Spec>
where
    DB: Database + 'db,
    E: Evm<
        DB = &'db mut State<DB>,
        Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
    >,
    R: OpReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt>,
    Spec: OpHardforks + Clone,
{
    /// Creates a new [`OpBlockExecutor`].
    pub fn new(evm: E, ctx: OpBlockExecutionCtx, spec: Spec, receipt_builder: R) -> Self {
        Self {
            is_regolith: spec
                .is_regolith_active_at_timestamp(evm.block().timestamp.saturating_to()),
            evm,
            system_caller: SystemCaller::new(spec.clone()),
            spec,
            receipt_builder,
            receipts: Vec::new(),
            gas_used: 0,
            ctx,
        }
    }

    /// Extends the [`BundleState`] of the executor with a specified pre-image.
    ///
    /// This should be used _only_ when initializing the executor
    pub fn with_bundle_prestate(mut self, pre_state: BundleState) -> Self {
        self.evm_mut().db_mut().bundle_state.extend(pre_state);
        self
    }

    /// Extends the receipts to reflect the aggregated execution result
    pub fn with_receipts(mut self, receipts: Vec<R::Receipt>) -> Self {
        self.receipts.extend_from_slice(&receipts);
        self
    }

    /// Extends the gas used to reflect the aggregated execution result
    pub fn with_gas_used(mut self, gas_used: u64) -> Self {
        self.gas_used += gas_used;
        self
    }
}

impl<'db, DB, E, R, Spec> BlockExecutor for FlashblocksBlockExecutor<E, R, Spec>
where
    DB: Database + 'db,
    E: Evm<
        DB = &'db mut State<DB>,
        Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
    >,
    R: OpReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt>,
    Spec: OpHardforks,
{
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;
    type Evm = E;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        // Set state clear flag if the block is after the Spurious Dragon hardfork.
        let state_clear_flag = self
            .spec
            .is_spurious_dragon_active_at_block(self.evm.block().number.saturating_to());
        self.evm.db_mut().set_state_clear_flag(state_clear_flag);

        self.system_caller
            .apply_blockhashes_contract_call(self.ctx.parent_hash, &mut self.evm)?;
        self.system_caller
            .apply_beacon_root_contract_call(self.ctx.parent_beacon_block_root, &mut self.evm)?;

        // Ensure that the create2deployer is force-deployed at the canyon transition. Optimism
        // blocks will always have at least a single transaction in them (the L1 info transaction),
        // so we can safely assume that this will always be triggered upon the transition and that
        // the above check for empty blocks will never be hit on OP chains.
        //
        // If the canyon hardfork is active at the current timestamp, and it was not active at the
        // previous block timestamp (heuristically, block time is not perfectly constant at 2s), and the
        // chain is an optimism chain, then we need to force-deploy the create2 deployer contract.
        if self
            .spec
            .is_canyon_active_at_timestamp(self.evm.block().timestamp.saturating_to())
            && !self.spec.is_canyon_active_at_timestamp(
                self.evm
                    .block()
                    .timestamp
                    .saturating_to::<u64>()
                    .saturating_sub(2),
            )
        {
            // Load the create2 deployer account from the cache.
            let acc = self
                .evm
                .db_mut()
                .load_cache_account(CREATE_2_DEPLOYER_ADDR)
                .map_err(BlockExecutionError::other)?;

            // Update the account info with the create2 deployer codehash and bytecode.
            let mut acc_info = acc.account_info().unwrap_or_default();
            acc_info.code_hash = CREATE_2_DEPLOYER_CODEHASH;
            acc_info.code = Some(Bytecode::new_raw(Bytes::from_static(
                &CREATE_2_DEPLOYER_BYTECODE,
            )));

            // Convert the cache account back into a revm account and mark it as touched.
            let mut revm_acc: revm::state::Account = acc_info.into();
            revm_acc.mark_touch();

            // Commit the create2 deployer account to the database.
            self.evm_mut()
                .db_mut()
                .commit(HashMap::from_iter([(CREATE_2_DEPLOYER_ADDR, revm_acc)]));
            return Ok(());
        }

        Ok(())
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutableTx<Self>,
        f: impl FnOnce(&ExecutionResult<<Self::Evm as Evm>::HaltReason>) -> CommitChanges,
    ) -> Result<Option<u64>, BlockExecutionError> {
        let is_deposit = tx.tx().ty() == DEPOSIT_TRANSACTION_TYPE;

        // The sum of the transaction’s gas limit, Tg, and the gas utilized in this block prior,
        // must be no greater than the block’s gasLimit.
        let block_available_gas = self.evm.block().gas_limit - self.gas_used;
        if tx.tx().gas_limit() > block_available_gas && (self.is_regolith || !is_deposit) {
            return Err(
                BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                    transaction_gas_limit: tx.tx().gas_limit(),
                    block_available_gas,
                }
                .into(),
            );
        }

        // Cache the depositor account prior to the state transition for the deposit nonce.
        //
        // Note that this *only* needs to be done post-regolith hardfork, as deposit nonces
        // were not introduced in Bedrock. In addition, regular transactions don't have deposit
        // nonces, so we don't need to touch the DB for those.
        let depositor = (self.is_regolith && is_deposit)
            .then(|| {
                self.evm
                    .db_mut()
                    .load_cache_account(*tx.signer())
                    .map(|acc| acc.account_info().unwrap_or_default())
            })
            .transpose()
            .map_err(BlockExecutionError::other)?;

        let hash = tx.tx().trie_hash();

        // Execute transaction.
        let ResultAndState { result, state } = self
            .evm
            .transact(&tx)
            .map_err(move |err| BlockExecutionError::evm(err, hash))?;

        if !f(&result).should_commit() {
            return Ok(None);
        }

        self.system_caller
            .on_state(StateChangeSource::Transaction(self.receipts.len()), &state);

        let gas_used = result.gas_used();

        // append gas used
        self.gas_used += gas_used;

        self.receipts.push(
            match self.receipt_builder.build_receipt(ReceiptBuilderCtx {
                tx: tx.tx(),
                result,
                cumulative_gas_used: self.gas_used,
                evm: &self.evm,
                state: &state,
            }) {
                Ok(receipt) => receipt,
                Err(ctx) => {
                    let receipt = alloy_consensus::Receipt {
                        // Success flag was added in `EIP-658: Embedding transaction status code
                        // in receipts`.
                        status: Eip658Value::Eip658(ctx.result.is_success()),
                        cumulative_gas_used: self.gas_used,
                        logs: ctx.result.into_logs(),
                    };

                    self.receipt_builder
                        .build_deposit_receipt(OpDepositReceipt {
                            inner: receipt,
                            deposit_nonce: depositor.map(|account| account.nonce),
                            // The deposit receipt version was introduced in Canyon to indicate an
                            // update to how receipt hashes should be computed
                            // when set. The state transition process ensures
                            // this is only set for post-Canyon deposit
                            // transactions.
                            deposit_receipt_version: (is_deposit
                                && self.spec.is_canyon_active_at_timestamp(
                                    self.evm.block().timestamp.saturating_to(),
                                ))
                            .then_some(1),
                        })
                }
            },
        );

        self.evm.db_mut().commit(state);

        Ok(Some(gas_used))
    }

    fn finish(
        mut self,
    ) -> Result<(Self::Evm, BlockExecutionResult<R::Receipt>), BlockExecutionError> {
        let balance_increments =
            post_block_balance_increments::<Header>(&self.spec, self.evm.block(), &[], None);
        // increment balances
        self.evm
            .db_mut()
            .increment_balances(balance_increments.clone())
            .map_err(|_| BlockValidationError::IncrementBalanceFailed)?;
        // call state hook with changes due to balance increments.
        self.system_caller.try_on_state_with(|| {
            balance_increment_state(&balance_increments, self.evm.db_mut()).map(|state| {
                (
                    StateChangeSource::PostBlock(StateChangePostBlockSource::BalanceIncrements),
                    Cow::Owned(state),
                )
            })
        })?;

        let gas_used = self
            .receipts
            .last()
            .map(|r| r.cumulative_gas_used())
            .unwrap_or_default();
        Ok((
            self.evm,
            BlockExecutionResult {
                receipts: self.receipts,
                requests: Default::default(),
                gas_used,
            },
        ))
    }

    fn set_state_hook(&mut self, hook: Option<Box<dyn OnStateHook>>) {
        self.system_caller.with_state_hook(hook);
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        &mut self.evm
    }

    fn evm(&self) -> &Self::Evm {
        &self.evm
    }
}

/// Ethereum block executor factory.
#[derive(Debug, Clone)]
pub struct FlashblocksBlockExecutorFactory {
    inner: OpBlockExecutorFactory<OpRethReceiptBuilder, OpChainSpec>,
    pre_state: Option<BundleState>,
}

impl FlashblocksBlockExecutorFactory {
    /// Creates a new [`OpBlockExecutorFactory`] with the given spec, [`EvmFactory`], and
    /// [`OpReceiptBuilder`].
    pub const fn new(
        receipt_builder: OpRethReceiptBuilder,
        spec: OpChainSpec,
        evm_factory: OpEvmFactory,
    ) -> Self {
        Self {
            inner: OpBlockExecutorFactory::new(receipt_builder, spec, evm_factory),
            pre_state: None,
        }
    }

    /// Exposes the chain specification.
    pub const fn spec(&self) -> &OpChainSpec {
        self.inner.spec()
    }

    /// Exposes the EVM factory.
    pub const fn evm_factory(&self) -> &OpEvmFactory {
        self.inner.evm_factory()
    }

    pub const fn take_bundle(&mut self) -> Option<BundleState> {
        self.pre_state.take()
    }

    /// Sets the pre-state for the block executor factory.
    pub fn set_pre_state(&mut self, pre_state: BundleState) {
        self.pre_state = Some(pre_state);
    }
}

impl BlockExecutorFactory for FlashblocksBlockExecutorFactory {
    type EvmFactory = OpEvmFactory;
    type ExecutionCtx<'a> = OpBlockExecutionCtx;
    type Transaction = OpTransactionSigned;
    type Receipt = OpReceipt;

    fn evm_factory(&self) -> &Self::EvmFactory {
        self.inner.evm_factory()
    }

    fn create_executor<'a, DB, I>(
        &'a self,
        evm: <OpEvmFactory as EvmFactory>::Evm<&'a mut State<DB>, I>,
        ctx: Self::ExecutionCtx<'a>,
    ) -> impl BlockExecutorFor<'a, Self, DB, I>
    where
        DB: Database + 'a,
        I: revm::Inspector<<OpEvmFactory as EvmFactory>::Context<&'a mut State<DB>>> + 'a,
    {
        if let Some(pre_state) = &self.pre_state {
            return FlashblocksBlockExecutor::new(
                evm,
                ctx,
                self.spec().clone(),
                OpRethReceiptBuilder::default(),
            )
            .with_bundle_prestate(pre_state.clone()); // TODO: Terrible clone here
        }

        FlashblocksBlockExecutor::new(
            evm,
            ctx,
            self.spec().clone(),
            OpRethReceiptBuilder::default(),
        )
    }
}

/// Block builder for Optimism.
#[derive(Debug)]
pub struct FlashblocksBlockAssembler {
    inner: OpBlockAssembler<OpChainSpec>,
}

impl FlashblocksBlockAssembler {
    /// Creates a new [`OpBlockAssembler`].
    pub const fn new(chain_spec: Arc<OpChainSpec>) -> Self {
        Self {
            inner: OpBlockAssembler::new(chain_spec),
        }
    }
}

impl FlashblocksBlockAssembler {
    /// Builds a block for `input` without any bounds on header `H`.
    pub fn assemble_block<
        F: for<'a> BlockExecutorFactory<
            ExecutionCtx<'a> = OpBlockExecutionCtx,
            Transaction: SignedTransaction,
            Receipt: Receipt + DepositReceipt,
        >,
        H,
    >(
        &self,
        input: BlockAssemblerInput<'_, '_, F, H>,
    ) -> Result<Block<F::Transaction>, BlockExecutionError> {
        self.inner.assemble_block(input)
    }
}

impl Clone for FlashblocksBlockAssembler {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<F> BlockAssembler<F> for FlashblocksBlockAssembler
where
    F: for<'a> BlockExecutorFactory<
        ExecutionCtx<'a> = OpBlockExecutionCtx,
        Transaction: SignedTransaction,
        Receipt: Receipt + DepositReceipt,
    >,
{
    type Block = Block<F::Transaction>;

    fn assemble_block(
        &self,
        input: BlockAssemblerInput<'_, '_, F>,
    ) -> Result<Self::Block, BlockExecutionError> {
        self.assemble_block(input)
    }
}

/// A wrapper around the [`BasicBlockBuilder`] for flashblocks.
pub struct FlashblocksBlockBuilder<'a, N: NodePrimitives, Evm> {
    pub inner: BasicBlockBuilder<
        'a,
        FlashblocksBlockExecutorFactory,
        FlashblocksBlockExecutor<Evm, OpRethReceiptBuilder, OpChainSpec>,
        OpBlockAssembler<OpChainSpec>,
        N,
    >,
}

impl<'a, N: NodePrimitives, Evm> FlashblocksBlockBuilder<'a, N, Evm> {
    /// Creates a new [`FlashblocksBlockBuilder`] with the given executor factory and assembler.
    pub fn new(
        ctx: OpBlockExecutionCtx,
        parent: &'a SealedHeader<N::BlockHeader>,
        executor: FlashblocksBlockExecutor<Evm, OpRethReceiptBuilder, OpChainSpec>,
        transactions: Vec<Recovered<N::SignedTx>>,
        chain_spec: Arc<OpChainSpec>,
    ) -> Self {
        Self {
            inner: BasicBlockBuilder {
                executor,
                assembler: OpBlockAssembler::new(chain_spec),
                ctx,
                parent,
                transactions,
            },
        }
    }
}

impl<'a, DB, N, E> BlockBuilder for FlashblocksBlockBuilder<'a, N, E>
where
    DB: Database + 'a,
    N: NodePrimitives<
        Receipt = OpReceipt,
        SignedTx = OpTransactionSigned,
        Block = alloy_consensus::Block<OpTransactionSigned>,
        BlockHeader = alloy_consensus::Header,
    >,
    E: Evm<
        DB = &'a mut State<DB>,
        Tx: FromRecoveredTx<OpTransactionSigned> + FromTxWithEncoded<OpTransactionSigned>,
        Spec = OpSpecId,
        HaltReason = OpHaltReason,
    >,
{
    type Primitives = N;
    type Executor = FlashblocksBlockExecutor<E, OpRethReceiptBuilder, OpChainSpec>;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.inner.apply_pre_execution_changes()
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutorTx<Self::Executor>,
        f: impl FnOnce(
            &ExecutionResult<<<Self::Executor as BlockExecutor>::Evm as Evm>::HaltReason>,
        ) -> CommitChanges,
    ) -> Result<Option<u64>, BlockExecutionError> {
        self.inner.execute_transaction_with_commit_condition(tx, f)
    }

    fn finish(
        self,
        state: impl StateProvider,
    ) -> Result<BlockBuilderOutcome<N>, BlockExecutionError> {
        self.inner.finish(state)
    }

    fn executor_mut(&mut self) -> &mut Self::Executor {
        self.inner.executor_mut()
    }

    fn executor(&self) -> &Self::Executor {
        self.inner.executor()
    }

    fn into_executor(self) -> Self::Executor {
        self.inner.into_executor()
    }
}

/// The current state of all known pre confirmations received over the P2P layer
/// or generated from the payload building job of this node.
///
/// The state is flushed when FCU is received with a parent hash that matches the block hash
/// of the latest pre confirmation _or_ when an FCU is received that does not match the latest pre confirmation,
/// in which case the pre confirmations were not included as part of the canonical chain.
#[derive(Debug, Clone)]
pub struct FlashblocksStateExecutor {
    inner: Arc<RwLock<FlashblocksStateExecutorInner>>,
    p2p_handle: FlashblocksHandle,
    da_config: OpDAConfig,
}

#[derive(Debug, Clone)]
pub struct FlashblocksStateExecutorInner {
    flashblocks: Flashblocks,
    latest_payload: Option<(OpBuiltPayload, u64)>,
}

impl FlashblocksStateExecutor {
    /// Creates a new instance of [`FlashblocksStateExecutor`].
    ///
    /// This function spawn a task that handles updates. It should only be called once.
    pub fn new(p2p_handle: FlashblocksHandle, da_config: OpDAConfig) -> Self {
        let inner = Arc::new(RwLock::new(FlashblocksStateExecutorInner {
            flashblocks: Flashblocks::default(),
            latest_payload: None,
        }));

        Self {
            inner,
            p2p_handle,
            da_config,
        }
    }

    /// Launches the executor to listen for new flashblocks and build payloads.
    pub fn launch<Node, P, Tx>(
        &self,
        ctx: &BuilderContext<Node>,
        payload_builder_ctx_builder: P,
        evm_config: OpEvmConfig,
    ) where
        Tx: OpPooledTx,
        Node: FullNodeTypes,
        Node::Provider: InMemoryState<Primitives = OpPrimitives>, // + FullNodeComponents<Evm = OpEvmConfig>,
        Node::Types: NodeTypes<ChainSpec = OpChainSpec>,
        P: PayloadBuilderCtxBuilder<OpEvmConfig, OpChainSpec, Tx> + 'static,
    {
        let mut stream = self.p2p_handle.flashblock_stream();
        let this = self.clone();
        let provider = ctx.provider().clone();
        let chain_spec = ctx.chain_spec().clone();
        ctx.task_executor()
            .spawn_critical("flashblocks executor", async move {
                while let Some(flashblock) = stream.next().await {
                    let mut state = this.inner.write();
                    if flashblock.index == 0 {
                        // Since flahblocks are pushed strictly in order, we can always clear
                        // self.latest_payload = None;
                        if flashblock.base.is_none() {
                            error!("first flashblock must have a base")
                        }
                        state.flashblocks.0.clear();
                    } else {
                        if flashblock.index != state.flashblocks.0.len() as u64 {
                            error!("flashblocks must be pushed in order")
                        }
                        if let Some(this) = state.flashblocks.0.first() {
                            if this.flashblock.payload_id != flashblock.payload_id {
                                error!("flashblock payload id must be sequential")
                            }
                        }
                    }

                    let flashblock = Flashblock { flashblock };
                    state.flashblocks.0.push(flashblock.clone());

                    let cancel = CancelOnDrop::default();

                    let first = &state.flashblocks.0.first().as_ref().unwrap().flashblock;
                    let base = first.base.as_ref().unwrap();

                    let transactions = flashblock
                        .flashblock
                        .diff
                        .transactions
                        .into_iter()
                        .map(|b| {
                            let tx: OpTxEnvelope = Decodable2718::decode_2718_exact(&b)?;
                            eyre::Result::Ok(WithEncoded::new(b.clone(), tx))
                        })
                        .collect::<eyre::Result<Vec<_>>>()
                        .unwrap();

                    let eth_attrs = EthPayloadBuilderAttributes {
                        id: first.payload_id,
                        parent: base.parent_hash,
                        timestamp: base.timestamp,
                        suggested_fee_recipient: base.fee_recipient,
                        prev_randao: base.prev_randao,
                        withdrawals: Withdrawals(first.diff.withdrawals.clone()),
                        parent_beacon_block_root: Some(base.parent_beacon_block_root),
                    };

                    let attributes = OpPayloadBuilderAttributes {
                        payload_attributes: eth_attrs,
                        no_tx_pool: false,
                        transactions: transactions.clone(),
                        gas_limit: None,
                        eip_1559_params: None,
                    };

                    let header = Header {
                        parent_hash: base.parent_hash,
                        ommers_hash: EMPTY_OMMER_ROOT_HASH,
                        beneficiary: Default::default(),
                        state_root: first.diff.state_root,
                        transactions_root: EMPTY_ROOT_HASH,
                        receipts_root: first.diff.receipts_root,
                        logs_bloom: first.diff.logs_bloom,
                        difficulty: Default::default(),
                        number: base.block_number,
                        gas_limit: base.gas_limit,
                        gas_used: first.diff.gas_used,
                        timestamp: base.timestamp,
                        extra_data: base.extra_data.clone(),
                        mix_hash: Default::default(),
                        nonce: B64::ZERO,
                        base_fee_per_gas: None,
                        withdrawals_root: Some(first.diff.withdrawals_root),
                        blob_gas_used: None,
                        excess_blob_gas: None,
                        parent_beacon_block_root: Some(base.parent_beacon_block_root),
                        requests_hash: None,
                    };
                    let sealed_header = Arc::new(SealedHeader::new_unhashed(header));

                    let config = PayloadConfig::new(sealed_header, attributes);

                    let builder_ctx = payload_builder_ctx_builder.build(
                        evm_config.clone(),
                        this.da_config.clone(),
                        chain_spec.clone(),
                        config,
                        &cancel,
                        state.latest_payload.as_ref().map(|p| p.0.clone()),
                    );

                    let best = |_| BestPayloadTransactions::new(vec![].into_iter());

                    let outcome = FlashblockBuilder::new(best)
                        .build(
                            provider.state_by_block_hash(base.parent_hash).unwrap(),
                            &builder_ctx,
                            state.latest_payload.as_ref().map(|p| p.0.clone()),
                        )
                        .unwrap();
                    let payload = match outcome {
                        BuildOutcomeKind::Better { payload } => payload,
                        BuildOutcomeKind::Freeze(payload) => payload,
                        _ => continue,
                    };
                    state.latest_payload = Some((payload.clone(), flashblock.flashblock.index));
                    provider
                        .in_memory_state()
                        .set_pending_block(payload.executed_block().unwrap());
                }
            });
    }

    pub fn publish_built_payload(
        &self,
        authorized_payload: AuthorizedPayload<FlashblocksPayloadV1>,
        built_payload: OpBuiltPayload,
    ) -> eyre::Result<()> {
        let mut lock = self.inner.write();

        let AuthorizedMsg::FlashblocksPayloadV1(ref payload) = authorized_payload.authorized.msg
        else {
            unreachable!()
        };

        if payload.index == 0 {
            // Since flahblocks are pushed strictly in order, we can always clear
            if payload.base.is_none() {
                bail!("first flashblock must have a base")
            }
            lock.flashblocks.0.clear();
        } else {
            if payload.index != lock.flashblocks.0.len() as u64 {
                bail!("flashblocks must be pushed in order")
            }
            if let Some(this) = lock.flashblocks.0.first() {
                if this.flashblock.payload_id != payload.payload_id {
                    bail!("flashblock payload id must be sequential")
                }
            }
        }

        self.p2p_handle.publish_new(authorized_payload.clone())?;

        lock.flashblocks.0.push(Flashblock {
            flashblock: payload.clone(),
        });
        lock.latest_payload = Some((built_payload, payload.index));

        Ok(())
    }

    /// Returns a reference to the latest flashblock.
    pub fn last(&self) -> Option<Flashblock> {
        self.inner.read().flashblocks.0.last().cloned()
    }

    /// Returns a reference to the latest flashblock.
    pub fn flashblocks(&self) -> Flashblocks {
        self.inner.read().flashblocks.clone()
    }

    /// Clears the current state of flashblocks.
    pub fn clear(&self) {
        // TODO:verify that pending state is automatically flushed here
        self.inner.write().flashblocks.0.clear();
    }
}
