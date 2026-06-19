use crossbeam_channel::Receiver;
use reth_tasks::TaskExecutor;
use world_chain_witness::ExecutionWitnessHandle;

use crate::{BlockExecutionWitness, ProviderBounds};

/// Spawns a background task that drains `receiver`, materializing each captured
/// [`ExecutionWitnessRecord`](reth_revm::witness::ExecutionWitnessRecord) into a full
/// [`ExecutionWitness`](alloy_rpc_types_debug::ExecutionWitness)
pub fn spawn_witness_collector<P: ProviderBounds>(
    provider: P,
    cache: ExecutionWitnessHandle,
    receiver: Receiver<BlockExecutionWitness>,
    tasks: TaskExecutor,
) {
    tasks.spawn_critical_task("world-chain-witness-collector", async move {
        while let Ok(BlockExecutionWitness {
            block_number,
            record,
        }) = receiver.recv()
        {
            let parent = block_number.saturating_sub(1);
            let witness = provider.history_by_block_number(parent).and_then(|state| {
                record.into_execution_witness(&state, &provider, block_number, Default::default())
            });

            match witness {
                Ok(witness) => cache.insert(block_number, witness),
                Err(err) => tracing::error!(
                    target: "world_chain::witness",
                    block_number,
                    %err,
                    "failed to assemble execution witness; skipping",
                ),
            }
        }
    });
}
