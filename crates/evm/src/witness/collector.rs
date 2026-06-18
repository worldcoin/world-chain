use std::sync::Arc;

use alloy_consensus::Header;
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::Bytes;
use alloy_rpc_types_debug::ExecutionWitness;
use crossbeam_channel::Sender;
use reth_primitives_traits::{Block, BlockBody};
use reth_provider::{
    BlockReader, HeaderProvider, ProviderError, ProviderResult, StateProviderFactory,
};
use reth_revm::witness::ExecutionWitnessRecord;
use world_chain_witness::{BlockWitness, WitnessCache};

use super::CapturedBlock;

/// Spawns a background thread that turns each [`CapturedBlock`] into a [`BlockWitness`] and inserts
/// it into the shared [`WitnessCache`].
///
/// Assembly is intentionally blocking (state-proof generation and block reads go through the
/// synchronous reth provider / trie), so this runs on a dedicated [`std::thread`] rather than the
/// async runtime. Returns the [`Sender`] half of an unbounded channel; the capturing EVM config
/// holds the sender and the collector owns the receiver.
///
/// Per-block assembly errors are logged and skipped; a single failed block never tears down the
/// collector loop. The thread exits cleanly once every [`Sender`] is dropped.
pub fn spawn_witness_collector<P>(provider: P, cache: Arc<WitnessCache>) -> Sender<CapturedBlock>
where
    P: StateProviderFactory
        + HeaderProvider<Header = Header>
        + BlockReader<Block: Block<Body: BlockBody<Transaction: Encodable2718>>>
        + Clone
        + Send
        + Sync
        + 'static,
{
    let (tx, rx) = crossbeam_channel::unbounded::<CapturedBlock>();

    std::thread::Builder::new()
        .name("world-chain-witness-collector".to_string())
        .spawn(move || {
            while let Ok(captured) = rx.recv() {
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
        })
        .expect("failed to spawn witness collector thread");

    tx
}

/// Assembles a [`BlockWitness`] from a [`CapturedBlock`] by resolving the pre-state proofs and the
/// block's header and transactions from the provider.
///
/// Mirrors the stock reth witness assembly
/// ([`ExecutionWitnessRecord::into_execution_witness`]): state-trie nodes come from the parent
/// state provider, and ancestor headers are pulled for the BLOCKHASH range.
fn assemble_block_witness<P>(provider: &P, captured: CapturedBlock) -> ProviderResult<BlockWitness>
where
    P: StateProviderFactory
        + HeaderProvider<Header = Header>
        + BlockReader<Block: Block<Body: BlockBody<Transaction: Encodable2718>>>,
{
    let CapturedBlock {
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
