use crossbeam_channel::Sender;
use reth_evm::{
    block::{BlockExecutorFactory, BlockExecutorFor, StateDB},
    ConfigureEvm, EvmFactory,
};
use revm::Inspector;

use crate::{execution::WorldChainBlockExecutor, BlockExecutionWitness};

/// A [`BlockExecutorFactory`] that wraps the executors produced by `E`'s block executor factory in
/// a [`WorldChainBlockExecutor`], threading through an optional capture channel.
#[derive(Debug, Clone)]
pub struct WorldChainBlockExecutorFactory<E: ConfigureEvm + 'static> {
    /// The inner factory whose executors are wrapped.
    pub(crate) inner: E::BlockExecutorFactory,
    /// Optional channel that receives a [`BlockExecutionWitness`] for every executed block.
    pub(crate) sender: Option<Sender<BlockExecutionWitness>>,
}

impl<E: ConfigureEvm + 'static> WorldChainBlockExecutorFactory<E> {
    /// Creates a new [`WorldChainBlockExecutorFactory`] over the given inner factory.
    pub const fn new(
        inner: E::BlockExecutorFactory,
        sender: Option<Sender<BlockExecutionWitness>>,
    ) -> Self {
        Self { inner, sender }
    }
}

impl<E: ConfigureEvm + 'static> BlockExecutorFactory for WorldChainBlockExecutorFactory<E> {
    type EvmFactory = <E::BlockExecutorFactory as BlockExecutorFactory>::EvmFactory;
    type TxExecutionResult = <E::BlockExecutorFactory as BlockExecutorFactory>::TxExecutionResult;
    type ExecutionCtx<'a> = <E::BlockExecutorFactory as BlockExecutorFactory>::ExecutionCtx<'a>;
    type Transaction = <E::BlockExecutorFactory as BlockExecutorFactory>::Transaction;
    type Receipt = <E::BlockExecutorFactory as BlockExecutorFactory>::Receipt;

    type Executor<'a, DB, I>
        = WorldChainBlockExecutor<BlockExecutorFor<'a, E::BlockExecutorFactory, DB, I>>
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
        WorldChainBlockExecutor {
            inner: self.inner.create_executor(evm, ctx),
            sender: self.sender.clone(),
        }
    }
}
