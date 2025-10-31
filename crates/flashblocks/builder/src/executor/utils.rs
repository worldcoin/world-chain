use flashblocks_primitives::access_list::FlashblockAccessList;
use reth_evm::block::BlockExecutionError;

/// Applys Post Execution changes to the [`FlashblockAccessList`] 
pub(crate) fn apply_post_execution_changes_to_access_list(
    access_list: &mut FlashblockAccessList,
    state: &revm::state::EvmState,
) -> Result<(), BlockExecutionError> {

    Ok(())
}

/// Applys Pre Execution changes to the [`FlashblockAccessList`]
pub(crate) fn apply_pre_execution_changes_to_access_list(
    access_list: &mut FlashblockAccessList,
    state: &revm::state::EvmState,
) -> Result<(), BlockExecutionError> {

    Ok(())
}