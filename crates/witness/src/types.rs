//! Wire and storage types for the live pre-image witness oracle.

use alloy_primitives::{B256, Bytes};
use alloy_rpc_types_debug::ExecutionWitness;
use serde::{Deserialize, Serialize};

/// The pre-image witness captured for a single committed L2 block.
///
/// Holds everything the Kona host needs to answer the L2-state portion of a range proof for this
/// block without re-deriving it from RPC: the execution witness (state trie nodes, contract
/// bytecode and unhashed account/slot keys), the RLP-encoded header, and the block's
/// RLP-encoded transactions (consumed by the host to rebuild the L2 transaction trie).
///
/// The boundary output root is reconstructable from `header_rlp` alone: post-Isthmus the
/// `L2ToL1MessagePasser` storage root is carried in the header's `withdrawals_root`, and the
/// message-passer storage nodes are already present in `execution_witness.state` (reth
/// force-loads that account post-Isthmus).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockWitness {
    /// Block number this witness was captured for.
    pub block_number: u64,
    /// Hash of the block this witness was captured for.
    pub block_hash: B256,
    /// Execution witness recorded from the live revm state during block import.
    pub execution_witness: ExecutionWitness,
    /// RLP-encoded block header.
    pub header_rlp: Bytes,
    /// RLP-encoded transactions, in block order.
    pub transactions: Vec<Bytes>,
}

/// The aggregated pre-image witness for an L2 block range `(start_block, end_block]`, returned by
/// `debug_collectRangeWitness`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RangeWitness {
    /// Exclusive lower bound of the range (the agreed parent block).
    pub start_block: u64,
    /// Inclusive upper bound of the range (the claimed block).
    pub end_block: u64,
    /// Per-block witnesses for every block in `(start_block, end_block]`, in ascending order.
    pub blocks: Vec<BlockWitness>,
}

#[cfg(test)]
mod tests {
    use alloy_primitives::bytes;

    use super::*;

    #[test]
    fn range_witness_serde_round_trip() {
        let range = RangeWitness {
            start_block: 100,
            end_block: 102,
            blocks: vec![
                BlockWitness {
                    block_number: 101,
                    block_hash: B256::repeat_byte(0x11),
                    execution_witness: ExecutionWitness {
                        state: vec![bytes!("deadbeef")],
                        codes: vec![bytes!("6001")],
                        keys: vec![bytes!("abcd")],
                        headers: vec![],
                    },
                    header_rlp: bytes!("c0"),
                    transactions: vec![bytes!("02")],
                },
                BlockWitness {
                    block_number: 102,
                    block_hash: B256::repeat_byte(0x12),
                    execution_witness: ExecutionWitness::default(),
                    header_rlp: bytes!("c1"),
                    transactions: vec![],
                },
            ],
        };

        let json = serde_json::to_string(&range).unwrap();
        let decoded: RangeWitness = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.start_block, range.start_block);
        assert_eq!(decoded.end_block, range.end_block);
        assert_eq!(decoded.blocks.len(), 2);
        assert_eq!(decoded.blocks[0].block_hash, B256::repeat_byte(0x11));
        assert_eq!(
            decoded.blocks[0].execution_witness.state,
            vec![bytes!("deadbeef")]
        );
        assert_eq!(decoded.blocks[0].transactions, vec![bytes!("02")]);
    }
}
