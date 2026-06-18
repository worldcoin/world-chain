use crossbeam_channel::Sender;
use reth_evm::{
    ConfigureEvm, Evm, EvmFactory,
    block::{BlockExecutorFactory, BlockExecutorFor, StateDB},
};
use revm::{Inspector, context::Block};

use super::{CapturedBlock, WitnessExecutor};
use crate::WorldChainEvmConfig;

/// The concrete [`BlockExecutorFactory`] used by [`WorldChainEvmConfig`].
type InnerFactory = <WorldChainEvmConfig as ConfigureEvm>::BlockExecutorFactory;

/// A [`BlockExecutorFactory`] that wraps the executors produced by [`WorldChainEvmConfig`]'s
/// factory in a [`WitnessExecutor`], threading through an optional capture channel.
#[derive(Debug, Clone)]
pub struct WitnessBlockExecutorFactory {
    /// The inner factory whose executors are wrapped.
    pub(crate) inner: InnerFactory,
    /// Optional channel that receives a [`CapturedBlock`] for every executed block.
    pub(crate) sender: Option<Sender<CapturedBlock>>,
}

impl WitnessBlockExecutorFactory {
    /// Creates a new [`WitnessBlockExecutorFactory`] over the given inner factory.
    pub const fn new(inner: InnerFactory, sender: Option<Sender<CapturedBlock>>) -> Self {
        Self { inner, sender }
    }
}

impl BlockExecutorFactory for WitnessBlockExecutorFactory {
    type EvmFactory = <InnerFactory as BlockExecutorFactory>::EvmFactory;
    type TxExecutionResult = <InnerFactory as BlockExecutorFactory>::TxExecutionResult;
    type ExecutionCtx<'a> = <InnerFactory as BlockExecutorFactory>::ExecutionCtx<'a>;
    type Transaction = <InnerFactory as BlockExecutorFactory>::Transaction;
    type Receipt = <InnerFactory as BlockExecutorFactory>::Receipt;

    type Executor<'a, DB, I>
        = WitnessExecutor<BlockExecutorFor<'a, InnerFactory, DB, I>>
    where
        DB: StateDB,
        I: Inspector<<Self::EvmFactory as EvmFactory>::Context<DB>>;

    fn evm_factory(&self) -> &Self::EvmFactory {
        self.inner.evm_factory()
    }

    fn create_executor<'a, DB, I>(
        &'a self,
        evm: <Self::EvmFactory as EvmFactory>::Evm<DB, I>,
        ctx: Self::ExecutionCtx<'a>,
    ) -> Self::Executor<'a, DB, I>
    where
        DB: StateDB,
        I: Inspector<<Self::EvmFactory as EvmFactory>::Context<DB>>,
    {
        // Read the block number from the EVM before it is moved into the inner executor.
        let block_number = evm.block().number().to::<u64>();

        WitnessExecutor {
            inner: self.inner.create_executor(evm, ctx),
            sender: self.sender.clone(),
            block_number,
        }
    }
}
