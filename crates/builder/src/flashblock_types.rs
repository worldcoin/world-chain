use crate::{
    execution_strategy::{FlashblocksBalExecutionStrategy, FlashblocksLegacyExecutionStrategy},
    state_root_strategy::{AsyncStateRootStrategy, FlashblockTypes, SyncStateRootStrategy},
};
use reth_optimism_evm::OpEvmConfig;

/// BAL-enabled flashblock types: parallel execution with async state root.
pub struct BalFlashblockTypes;

impl FlashblockTypes<OpEvmConfig> for BalFlashblockTypes {
    type Execution = FlashblocksBalExecutionStrategy<AsyncStateRootStrategy>;
    type StateRoot = AsyncStateRootStrategy;
}

/// Legacy (non-BAL) flashblock types: sequential execution with inline state root.
pub struct LegacyFlashblockTypes;

impl FlashblockTypes<OpEvmConfig> for LegacyFlashblockTypes {
    type Execution = FlashblocksLegacyExecutionStrategy;
    type StateRoot = SyncStateRootStrategy;
}
