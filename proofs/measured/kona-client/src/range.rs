use alloy_primitives::B256;
use kona_protocol::OutputRoot;
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
    fn as_output_root(&self) -> OutputRoot {
        OutputRoot::from_parts(
            self.state_root,
            self.message_passer_storage_root,
            self.block_hash,
        )
    }

    /// Encodes the versioned OP Stack output-root preimage.
    pub fn encode(&self) -> [u8; 128] {
        self.as_output_root().encode()
    }

    /// Computes the OP Stack output root:
    /// `keccak256(version || state_root || message_passer_storage_root || block_hash)`.
    pub fn output_root(&self) -> B256 {
        self.as_output_root().hash()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::b256;

    #[test]
    fn preserves_v0_output_encoding_and_hash() {
        let witness = OutputRootWitness {
            state_root: B256::left_padding_from(&[0xbe, 0xef]),
            message_passer_storage_root: B256::left_padding_from(&[0xba, 0xbe]),
            block_hash: B256::left_padding_from(&[0xc0, 0xde]),
        };
        let encoded = witness.encode();
        assert_eq!(&encoded[..32], &[0; 32]);
        assert_eq!(&encoded[32..64], witness.state_root.as_slice());
        assert_eq!(
            &encoded[64..96],
            witness.message_passer_storage_root.as_slice()
        );
        assert_eq!(&encoded[96..], witness.block_hash.as_slice());
        assert_eq!(
            witness.output_root(),
            b256!("0c39fb6b07cf6694b13e63e59f7b15255be1c93a4d6d3e0da6c99729647c0d11")
        );
    }
}
