use crate::{
    execution_strategy::{FlashblocksBalExecutionStrategy, FlashblocksLegacyExecutionStrategy},
    state_root_strategy::{AsyncStateRootStrategy, FlashblockTypes},
};
use reth_optimism_evm::OpEvmConfig;

/// BAL-enabled flashblock types: parallel execution with parallel state root.
pub struct BalFlashblockTypes;

impl FlashblockTypes<OpEvmConfig> for BalFlashblockTypes {
    type Execution = FlashblocksBalExecutionStrategy<AsyncStateRootStrategy>;
}

/// Legacy (non-BAL) flashblock types: sequential execution, no external state root.
pub struct LegacyFlashblockTypes;

impl FlashblockTypes<OpEvmConfig> for LegacyFlashblockTypes {
    type Execution = FlashblocksLegacyExecutionStrategy;
}
