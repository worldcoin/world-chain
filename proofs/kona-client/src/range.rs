use alloy_primitives::{B256, keccak256};
use serde::{Deserialize, Serialize};

/// Data needed to recompute an OP Stack output root for one L2 block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputRootWitness {
    /// L2 state root from the execution payload/header.
    pub state_root: B256,
    /// Storage root of the `L2ToL1MessagePasser` predeploy at this block.
    pub message_passer_storage_root: B256,
    /// L2 block hash.
    pub block_hash: B256,
}

impl OutputRootWitness {
    /// Computes the OP Stack output root:
    /// `keccak256(version || state_root || message_passer_storage_root || block_hash)`.
    pub fn output_root(&self) -> B256 {
        let mut preimage = [0u8; 128];
        preimage[32..64].copy_from_slice(self.state_root.as_slice());
        preimage[64..96].copy_from_slice(self.message_passer_storage_root.as_slice());
        preimage[96..128].copy_from_slice(self.block_hash.as_slice());
        keccak256(preimage)
    }
}
