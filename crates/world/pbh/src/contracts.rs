use IPBHEntryPoint::PBHPayload as IPBHPayload;
use alloy_sol_types::sol;
use serde::{Deserialize, Serialize};

use crate::{
    external_nullifier::{EncodedExternalNullifier, ExternalNullifier},
    payload::{PBHPayload, Proof},
};

sol! {
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

        function spendNullifierHashes(uint256[] memory _nullifierHashes) external;
        function numPbhPerMonth() external view returns (uint16);
        function nullifierHashes(uint256 nullifierHash) external view returns (uint256);
    }
}

impl TryFrom<IPBHPayload> for PBHPayload {
    type Error = alloy_rlp::Error;

    fn try_from(val: IPBHPayload) -> Result<Self, Self::Error> {
        let proof = Proof(semaphore_rs::protocol::Proof::from_flat(val.proof));

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
