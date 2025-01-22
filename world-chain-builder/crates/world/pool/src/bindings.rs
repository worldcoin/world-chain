use alloy_sol_types::sol;
use world_chain_builder_pbh::{
    external_nullifier::ExternalNullifier,
    payload::{PbhPayload, Proof},
};
use IPBHEntryPoint::PBHPayload;

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
        #[derive(Default)]
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

impl Into<PbhPayload> for PBHPayload {
    fn into(self) -> PbhPayload {
        let proof: [ethers_core::types::U256; 8] = self
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

        PbhPayload {
            external_nullifier: ExternalNullifier::from_word(self.pbhExternalNullifier),
            nullifier_hash: self.nullifierHash,
            root: self.root,
            proof,
        }
    }
}
