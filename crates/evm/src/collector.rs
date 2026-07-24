use std::collections::BTreeMap;

use alloy_consensus::BlockHeader;
use crossbeam_channel::{Receiver, TryRecvError};
use futures::StreamExt;
use reth_provider::CanonStateSubscriptions;
use reth_revm::witness::ExecutionWitnessRecord;
use reth_tasks::TaskExecutor;

use crate::{BlockExecutionWitness, ExecutionWitnessHandle, ProviderBounds};

/// Spawns the live witness collector as a critical task.
pub fn spawn_witness_collector<P>(
    provider: P,
    cache: ExecutionWitnessHandle,
    witnesses: Receiver<BlockExecutionWitness>,
    tasks: &TaskExecutor,
) where
    P: ProviderBounds + CanonStateSubscriptions,
{
    let mut canon = provider.canonical_state_stream();

    tasks.spawn_critical_task("world-chain-witness-collector", async move {
        // Captured records awaiting their block becoming canonical, keyed by block number.
        let mut queued: BTreeMap<u64, ExecutionWitnessRecord> = BTreeMap::new();

        while let Some(notification) = canon.next().await {
            // Drain the channel without blocking.
            loop {
                match witnesses.try_recv() {
                    Ok(BlockExecutionWitness {
                        block_number,
                        record,
                    }) => {
                        queued.insert(block_number, record);
                    }
                    Err(TryRecvError::Empty) => break,
                    // The capturing EVM config was dropped: the node is shutting down.
                    Err(TryRecvError::Disconnected) => return,
                }
            }

            // Assemble every queued witness whose block is now canonical; keep newer ones buffered.
            let tip = notification.tip().header().number();
            let (ready, future): (BTreeMap<u64, _>, BTreeMap<u64, _>) = std::mem::take(&mut queued)
                .into_iter()
                .partition(|(block_number, _)| *block_number <= tip);
            queued = future;

            if ready.is_empty() {
                continue;
            }

            let provider = provider.clone();
            let cache = cache.clone();

            tokio::task::spawn_blocking(move || {
                for (block_number, record) in ready {
                    let parent = block_number.saturating_sub(1);
                    let result = provider.history_by_block_number(parent).and_then(|state| {
                        record.into_execution_witness(
                            &state,
                            &provider,
                            block_number,
                            Default::default(),
                        )
                    });
                    match result {
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
    });
}
