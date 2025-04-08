use reth_evm::block::BlockExecutionError;
use reth_evm::execute::BlockBuilderOutcome;
use reth_primitives::NodePrimitives;
use reth_provider::StateProvider;

// TODO: impl Flashblock for BasicBlock builder
pub trait Flashblock<N: NodePrimitives> {
    fn finish_flashblock(
        self,
        state: impl StateProvider,
    ) -> Result<BlockBuilderOutcome<N>, BlockExecutionError>;
}
