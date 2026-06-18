use crossbeam_channel::Sender;
use reth_evm::{
    ConfigureEngineEvm, ConfigureEvm, Database, EvmEnvFor, ExecutableTxIterator, ExecutionCtxFor,
};
use reth_optimism_evm::{ConfigurePostExecEvm, PostExecMode};
use reth_optimism_payload_builder::OpExecData;
use reth_primitives_traits::{NodePrimitives, SealedBlock, SealedHeader};
use reth_revm::{State, witness::ExecutionWitnessRecord};

use super::WitnessBlockExecutorFactory;
use crate::WorldChainEvmConfig;

/// A captured execution witness for a single block.
#[derive(Debug, Clone)]
pub struct CapturedBlock {
    /// Number of the block this witness was captured for.
    pub block_number: u64,
    /// The recorded execution witness.
    pub record: ExecutionWitnessRecord,
}

/// A newtype over [`WorldChainEvmConfig`] that captures each block's execution witness during block
/// execution.
///
/// All [`ConfigureEvm`] and [`ConfigurePostExecEvm`] behavior is delegated to the inner config,
/// except that the block executor factory is replaced with a [`WitnessBlockExecutorFactory`] that
/// snapshots execution state on `finish`. When constructed without a sender, capturing is disabled
/// and the wrapper behaves as a pure passthrough.
#[derive(Debug, Clone)]
pub struct WitnessCapturingEvmConfig {
    /// The wrapped node EVM configuration.
    inner: WorldChainEvmConfig,
    /// The witness-capturing block executor factory built from `inner`'s factory.
    factory: WitnessBlockExecutorFactory,
}

impl WitnessCapturingEvmConfig {
    /// Creates a new [`WitnessCapturingEvmConfig`] over the given config.
    ///
    /// When `sender` is [`Some`], every executed block's [`CapturedBlock`] is forwarded over the
    /// channel. When [`None`], capturing is disabled.
    pub fn new(inner: WorldChainEvmConfig, sender: Option<Sender<CapturedBlock>>) -> Self {
        let factory =
            WitnessBlockExecutorFactory::new(inner.block_executor_factory().clone(), sender);
        Self { inner, factory }
    }

    /// Returns a reference to the wrapped [`WorldChainEvmConfig`].
    pub const fn inner(&self) -> &WorldChainEvmConfig {
        &self.inner
    }

    /// Consumes the wrapper and returns the wrapped [`WorldChainEvmConfig`].
    ///
    /// Used at boundaries (e.g. the block-builder path) that operate on the inner config directly.
    pub fn into_inner(self) -> WorldChainEvmConfig {
        self.inner
    }
}

impl ConfigureEvm for WitnessCapturingEvmConfig {
    type Primitives = <WorldChainEvmConfig as ConfigureEvm>::Primitives;
    type Error = <WorldChainEvmConfig as ConfigureEvm>::Error;
    type NextBlockEnvCtx = <WorldChainEvmConfig as ConfigureEvm>::NextBlockEnvCtx;
    type BlockExecutorFactory = WitnessBlockExecutorFactory;
    type BlockAssembler = <WorldChainEvmConfig as ConfigureEvm>::BlockAssembler;

    fn block_executor_factory(&self) -> &Self::BlockExecutorFactory {
        &self.factory
    }

    fn block_assembler(&self) -> &Self::BlockAssembler {
        self.inner.block_assembler()
    }

    fn evm_env(
        &self,
        header: &<Self::Primitives as NodePrimitives>::BlockHeader,
    ) -> Result<EvmEnvFor<Self>, Self::Error> {
        self.inner.evm_env(header)
    }

    fn next_evm_env(
        &self,
        parent: &<Self::Primitives as NodePrimitives>::BlockHeader,
        attributes: &Self::NextBlockEnvCtx,
    ) -> Result<EvmEnvFor<Self>, Self::Error> {
        self.inner.next_evm_env(parent, attributes)
    }

    fn context_for_block<'a>(
        &self,
        block: &'a SealedBlock<<Self::Primitives as NodePrimitives>::Block>,
    ) -> Result<ExecutionCtxFor<'a, Self>, Self::Error> {
        self.inner.context_for_block(block)
    }

    fn context_for_next_block(
        &self,
        parent: &SealedHeader<<Self::Primitives as NodePrimitives>::BlockHeader>,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<ExecutionCtxFor<'_, Self>, Self::Error> {
        self.inner.context_for_next_block(parent, attributes)
    }
}

impl ConfigurePostExecEvm for WitnessCapturingEvmConfig {
    fn post_exec_executor_for_block<'a, DB: Database>(
        &'a self,
        db: &'a mut State<DB>,
        block: &'a SealedBlock<<Self::Primitives as NodePrimitives>::Block>,
        post_exec_mode: PostExecMode,
    ) -> Result<
        impl reth_evm::block::BlockExecutor<
            Transaction = <Self::Primitives as NodePrimitives>::SignedTx,
            Receipt = <Self::Primitives as NodePrimitives>::Receipt,
        > + reth_optimism_evm::PostExecExecutorExt
        + 'a,
        Self::Error,
    > {
        self.inner
            .post_exec_executor_for_block(db, block, post_exec_mode)
    }

    fn post_exec_builder_for_next_block<'a, DB: Database + 'a>(
        &'a self,
        db: &'a mut State<DB>,
        parent: &'a SealedHeader<<Self::Primitives as NodePrimitives>::BlockHeader>,
        attributes: Self::NextBlockEnvCtx,
        post_exec_mode: PostExecMode,
    ) -> Result<
        impl reth_evm::execute::BlockBuilder<
            Primitives = Self::Primitives,
            Executor: reth_optimism_evm::PostExecExecutorExt
                          + reth_evm::block::BlockExecutor<
                Evm: reth_evm::Evm<DB: core::ops::DerefMut<Target = State<DB>>>,
                Result: reth_optimism_evm::PreRefundGasUsed,
            >,
        > + 'a,
        Self::Error,
    > {
        self.inner
            .post_exec_builder_for_next_block(db, parent, attributes, post_exec_mode)
    }
}

impl ConfigureEngineEvm<OpExecData> for WitnessCapturingEvmConfig {
    fn evm_env_for_payload(&self, payload: &OpExecData) -> Result<EvmEnvFor<Self>, Self::Error> {
        self.inner.evm_env_for_payload(payload)
    }

    fn context_for_payload<'a>(
        &self,
        payload: &'a OpExecData,
    ) -> Result<ExecutionCtxFor<'a, Self>, Self::Error> {
        self.inner.context_for_payload(payload)
    }

    fn tx_iterator_for_payload(
        &self,
        payload: &OpExecData,
    ) -> Result<impl ExecutableTxIterator<Self>, Self::Error> {
        self.inner.tx_iterator_for_payload(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use world_chain_chainspec::WorldChainSpec;

    #[test]
    fn witness_capturing_config_passthrough_construction() {
        let config = WitnessCapturingEvmConfig::new(
            WorldChainEvmConfig::optimism(WorldChainSpec::dev()),
            None,
        );

        // The wrapper exposes its own witness-capturing factory.
        let _factory: &WitnessBlockExecutorFactory = config.block_executor_factory();
    }
}
