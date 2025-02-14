use alloy_sol_types::sol;
use serde::{Deserialize, Serialize};
use world_chain_builder_pbh::{
    external_nullifier::{EncodedExternalNullifier, ExternalNullifier},
    payload::{PBHPayload, Proof},
};
use IPBHEntryPoint::PBHPayload as IPBHPayload;

sol! {
    contract IMulticall3 {
        #[derive(Default)]
        struct Call3 {
            address target;
            bool allowFailure;
            bytes callData;
        }
    }

    contract IEntryPoint {
        #[derive(Default, Serialize, Deserialize, Debug)]
        struct PackedUserOperation {
            address sender;
            uint256 nonce;
            bytes initCode;
            bytes callData;
            bytes32 accountGasLimits;
            uint256 preVerificationGas;
            bytes32 gasFees;
            bytes paymasterAndData;
            bytes signature;
        }

        #[derive(Default)]
        struct UserOpsPerAggregator {
            PackedUserOperation[] userOps;
            address aggregator;
            bytes signature;
        }
    }

    contract IPBHEntryPoint {
        #[derive(Default)]
        struct PBHPayload {
            uint256 root;
            uint256 pbhExternalNullifier;
            uint256 nullifierHash;
            uint256[8] proof;
        }

        function handleAggregatedOps(
            IEntryPoint.UserOpsPerAggregator[] calldata,
            address payable
        ) external;

        function pbhMulticall(
            IMulticall3.Call3[] calls,
            PBHPayload payload,
        ) external;
    }
}

impl TryFrom<IPBHPayload> for PBHPayload {
    type Error = alloy_rlp::Error;

    fn try_from(val: IPBHPayload) -> Result<Self, Self::Error> {
        let proof: [ethers_core::types::U256; 8] = val
            .proof
            .into_iter()
            .map(|x| {
                // TODO: Switch to ruint in semaphore-rs and remove this
                let bytes_repr: [u8; 32] = x.to_be_bytes();
                ethers_core::types::U256::from_big_endian(&bytes_repr)
            })
            .collect::<Vec<_>>()
            .try_into()
            .unwrap(); // TODO: should we be unwrapping here?

        let g1a = (proof[0], proof[1]);
        let g2 = ([proof[2], proof[3]], [proof[4], proof[5]]);
        let g1b = (proof[6], proof[7]);

        let proof = Proof(semaphore::protocol::Proof(g1a, g2, g1b));

        Ok(PBHPayload {
            external_nullifier: ExternalNullifier::try_from(EncodedExternalNullifier(
                val.pbhExternalNullifier,
            ))?,
            nullifier_hash: val.nullifierHash,
            root: val.root,
            proof,
        })
    }
}
