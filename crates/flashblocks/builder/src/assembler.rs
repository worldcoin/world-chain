use alloy_consensus::Block;
use alloy_op_evm::OpBlockExecutionCtx;
use reth::core::primitives::Receipt;
use reth_evm::block::BlockExecutionError;
use reth_evm::block::BlockExecutorFactory;
use reth_evm::execute::{BlockAssembler, BlockAssemblerInput};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::OpBlockAssembler;
use reth_optimism_primitives::DepositReceipt;
use reth_primitives::transaction::SignedTransaction;
use std::sync::Arc;

/// Assembles the full block from the bundle state and Execution Result
#[derive(Clone, Debug)]
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
        self.inner.assemble_block(input)
    }
}
