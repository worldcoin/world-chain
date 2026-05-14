//! Public boot value ABI used by range proofs and the aggregation program.

use alloy_sol_types::sol;
use serde::{Deserialize, Serialize};

use alloy_primitives::B256;
use kona_proof::BootInfo;

/// Public boot values committed by the range proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootInfoPublicValues {
    /// L1 head used by the derivation pipeline.
    pub l1_head: B256,
    /// Agreed pre-state L2 output root.
    pub l2_pre_root: B256,
    /// Claimed post-state L2 output root.
    pub l2_post_root: B256,
    /// Claimed post-state L2 block number.
    pub l2_block_number: u64,
    /// OP Succinct rollup config hash.
    pub rollup_config_hash: B256,
}

impl BootInfoPublicValues {
    /// Creates boot public values from proof roots and an already computed rollup config hash.
    pub const fn new(
        l1_head: B256,
        l2_pre_root: B256,
        l2_post_root: B256,
        l2_block_number: u64,
        rollup_config_hash: B256,
    ) -> Self {
        Self {
            l1_head,
            l2_pre_root,
            l2_post_root,
            l2_block_number,
            rollup_config_hash,
        }
    }
}

sol! {
    /// OP Succinct-compatible range proof public values.
    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct BootInfoStruct {
        bytes32 l1Head;
        bytes32 l2PreRoot;
        bytes32 l2PostRoot;
        uint64 l2BlockNumber;
        bytes32 rollupConfigHash;
    }
}

impl From<BootInfoPublicValues> for BootInfoStruct {
    fn from(value: BootInfoPublicValues) -> Self {
        Self {
            l1Head: value.l1_head,
            l2PreRoot: value.l2_pre_root,
            l2PostRoot: value.l2_post_root,
            l2BlockNumber: value.l2_block_number,
            rollupConfigHash: value.rollup_config_hash,
        }
    }
}

impl BootInfoStruct {
    /// Converts Kona boot info into the on-chain public values using the World rollup config hash.
    pub fn from_kona_boot_info(boot_info: BootInfo, rollup_config_hash: B256) -> Self {
        Self {
            l1Head: boot_info.l1_head,
            l2PreRoot: boot_info.agreed_l2_output_root,
            l2PostRoot: boot_info.claimed_l2_output_root,
            l2BlockNumber: boot_info.claimed_l2_block_number,
            rollupConfigHash: rollup_config_hash,
        }
    }
}

impl From<&BootInfoPublicValues> for BootInfoStruct {
    fn from(value: &BootInfoPublicValues) -> Self {
        Self {
            l1Head: value.l1_head,
            l2PreRoot: value.l2_pre_root,
            l2PostRoot: value.l2_post_root,
            l2BlockNumber: value.l2_block_number,
            rollupConfigHash: value.rollup_config_hash,
        }
    }
}

impl From<BootInfoStruct> for BootInfoPublicValues {
    fn from(value: BootInfoStruct) -> Self {
        Self::new(
            value.l1Head,
            value.l2PreRoot,
            value.l2PostRoot,
            value.l2BlockNumber,
            value.rollupConfigHash,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;

    #[test]
    fn converts_world_boot_values_to_op_succinct_struct() {
        let values = BootInfoPublicValues::new(
            B256::from([1; 32]),
            B256::from([2; 32]),
            B256::from([3; 32]),
            42,
            B256::from([4; 32]),
        );

        let boot_info = BootInfoStruct::from(&values);

        assert_eq!(boot_info.l1Head, values.l1_head);
        assert_eq!(boot_info.l2PreRoot, values.l2_pre_root);
        assert_eq!(boot_info.l2PostRoot, values.l2_post_root);
        assert_eq!(boot_info.l2BlockNumber, values.l2_block_number);
        assert_eq!(boot_info.rollupConfigHash, values.rollup_config_hash);
    }
}
