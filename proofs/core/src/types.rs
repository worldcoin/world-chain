//! Aggregation program input and output types.

use alloy_primitives::{Address, B256};
use alloy_sol_types::sol;
use serde::{Deserialize, Serialize};

use crate::boot::BootInfoStruct;

/// Inputs consumed by the aggregation guest program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregationInputs {
    /// Public values from each range proof being aggregated.
    pub boot_infos: Vec<BootInfoStruct>,
    /// L1 checkpoint head that the supplied header chain must terminate at.
    pub latest_l1_checkpoint_head: B256,
    /// SP1 verifying key for the range proof program.
    pub multi_block_vkey: [u32; 8],
    /// Address of the prover that generated the aggregation proof.
    pub prover_address: Address,
}

sol! {
    /// ABI-encoded public values committed by the aggregation proof.
    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct AggregationOutputs {
        bytes32 l1Head;
        bytes32 l2PreRoot;
        bytes32 l2PostRoot;
        uint64 l2BlockNumber;
        bytes32 rollupConfigHash;
        bytes32 multiBlockVKey;
        address proverAddress;
    }
}

/// Converts an SP1 vkey digest from `[u32; 8]` to the bytes used on-chain.
pub fn u32_to_u8(input: [u32; 8]) -> [u8; 32] {
    std::array::from_fn(|i| input[i / 4].to_be_bytes()[i % 4])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_vkey_words_big_endian() {
        let words = [
            0x0102_0304,
            0x0506_0708,
            0x090a_0b0c,
            0x0d0e_0f10,
            0x1112_1314,
            0x1516_1718,
            0x191a_1b1c,
            0x1d1e_1f20,
        ];
        assert_eq!(
            u32_to_u8(words),
            [
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
                24, 25, 26, 27, 28, 29, 30, 31, 32,
            ]
        );
    }
}
