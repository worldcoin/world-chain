// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {DomainConfig, RootCommitment} from "../ProofSystemTypes.sol";

library ProofRootIdLib {
    function hashDomain(DomainConfig memory domain) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                domain.chainId,
                domain.proofSystemVersion,
                domain.rollupConfigHash,
                domain.blockInterval,
                domain.intermediateBlockInterval
            )
        );
    }

    function hashRootId(bytes32 domainHash, RootCommitment memory commitment) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                domainHash,
                commitment.parentRef,
                commitment.rootClaim,
                commitment.l2BlockNumber,
                commitment.intermediateRootsHash,
                commitment.l1OriginHash,
                commitment.l1OriginNumber
            )
        );
    }
}
