use std::sync::Arc;

use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::Bytes;
use alloy_rpc_types_debug::ExecutionWitness;
use crossbeam_channel::Receiver;
use reth_primitives_traits::{Block, BlockBody};
use reth_provider::{ProviderError, ProviderResult};
use reth_revm::witness::ExecutionWitnessRecord;
use reth_tasks::TaskExecutor;
use world_chain_witness::{BlockWitness, WitnessCache};

use crate::{BlockExecutionWitness, ProviderBounds};

/// Spawns a background thread that drains `receiver`, turning each [`BlockExecutionWitness`] into a
/// [`BlockWitness`] and inserting it into the shared [`WitnessCache`].
///
/// The matching [`Sender`](crossbeam_channel::Sender) is created upstream and handed to the
/// capturing EVM config, so this takes only the receiving half.
///
/// Assembly is intentionally blocking (state-proof generation and block reads go through the
/// synchronous reth provider / trie), so this runs on a dedicated [`std::thread`] rather than the
/// async runtime.
///
/// Per-block assembly errors are logged and skipped; a single failed block never tears down the
/// collector loop. The thread exits cleanly once every sender is dropped.
pub fn spawn_witness_collector<P: ProviderBounds>(
    provider: P,
    cache: Arc<WitnessCache>,
    receiver: Receiver<BlockExecutionWitness>,
    tasks: TaskExecutor,
) {
    tasks.spawn_critical_task("world-chain-witness-collector", async move {
        while let Ok(captured) = receiver.recv() {
            let block_number = captured.block_number;
            match assemble_block_witness(&provider, captured) {
                Ok(witness) => cache.insert(witness),
                Err(err) => {
                    tracing::warn!(
                        target: "world_chain::witness",
                        block_number,
                        %err,
                        "failed to assemble block witness; skipping",
                    );
                }
            }
        }
    });
}

/// Assembles a [`BlockWitness`] from a [`BlockExecutionWitness`] by resolving the pre-state proofs and the
/// block's header and transactions from the provider.
///
/// Mirrors the stock reth witness assembly
/// ([`ExecutionWitnessRecord::into_execution_witness`]): state-trie nodes come from the parent
/// state provider, and ancestor headers are pulled for the BLOCKHASH range.
fn assemble_block_witness<P: ProviderBounds>(
    provider: &P,
    captured: BlockExecutionWitness,
) -> ProviderResult<BlockWitness> {
    let BlockExecutionWitness {
        block_number,
        record,
    } = captured;

    let ExecutionWitnessRecord {
        hashed_state,
        codes,
        keys,
        lowest_block_number,
    } = record;

    // State-trie pre-images are proven against the parent (pre-state) of the captured block.
    let parent = block_number.saturating_sub(1);
    let state_provider = provider.history_by_block_number(parent)?;
    let state = state_provider.witness(Default::default(), hashed_state, Default::default())?;

    // Ancestor headers required by any BLOCKHASH opcode, in `[lowest, parent]`. Mirrors the stock
    // `into_execution_witness` range of `lowest..block_number`.
    let lowest = lowest_block_number.unwrap_or(parent);
    let headers = provider
        .headers_range(lowest..block_number)?
        .into_iter()
        .map(|header| Bytes::from(alloy_rlp::encode(&header)))
        .collect();

    let execution_witness = ExecutionWitness {
        state,
        codes,
        keys,
        headers,
    };

    let sealed = provider
        .sealed_header(block_number)?
        .ok_or(ProviderError::HeaderNotFound(block_number.into()))?;
    let block_hash = sealed.hash();
    let header_rlp = Bytes::from(alloy_rlp::encode(sealed.header()));

    let block = provider
        .block_by_number(block_number)?
        .ok_or(ProviderError::BlockBodyIndicesNotFound(block_number))?;
    let transactions = block
        .body()
        .transactions()
        .iter()
        .map(|tx| Bytes::from(tx.encoded_2718()))
        .collect();

    Ok(BlockWitness {
        block_number,
        block_hash,
        execution_witness,
        header_rlp,
        transactions,
    })
}
