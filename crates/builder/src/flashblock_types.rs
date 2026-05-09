use crate::{
    WorldChainEvmConfig,
    execution_strategy::{FlashblocksBalExecutionStrategy, FlashblocksLegacyExecutionStrategy},
    state_root_strategy::{AsyncStateRootStrategy, FlashblockTypes, SyncStateRootStrategy},
};

/// BAL-enabled flashblock types: parallel execution with async state root.
pub struct BalFlashblockTypes;

impl FlashblockTypes<WorldChainEvmConfig> for BalFlashblockTypes {
    type StateRoot = AsyncStateRootStrategy;
    type Execution = FlashblocksBalExecutionStrategy;
}

/// Legacy (non-BAL) flashblock types: sequential execution with inline state root.
pub struct LegacyFlashblockTypes;

impl FlashblockTypes<WorldChainEvmConfig> for LegacyFlashblockTypes {
    type StateRoot = SyncStateRootStrategy;
    type Execution = FlashblocksLegacyExecutionStrategy;
}
