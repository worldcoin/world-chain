// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {IProofLaneVerifier} from "./interfaces/IProofLaneVerifier.sol";
import {IStakingRegistry} from "./interfaces/IStakingRegistry.sol";
import {ProofRootIdLib} from "./libraries/ProofRootIdLib.sol";
import {ProofSystemGame} from "./ProofSystemGame.sol";
import {DomainConfig, RootCommitment} from "./ProofSystemTypes.sol";

contract ProofSystemFactory {
    uint256 public constant PROOF_LANE_COUNT = 4;

    DomainConfig public domainConfig;
    IProofLaneVerifier[4] public laneVerifiers;
    IStakingRegistry public immutable stakingRegistry;
    uint256 public immutable challengePeriod;
    uint256 public immutable bondAmount;

    address[] public games;

    error InvalidChallengePeriod();
    error MissingLaneVerifier(uint256 lane);
    error MissingStakingRegistry();

    event ProofSystemGameCreated(
        address indexed game,
        bytes32 indexed rootId,
        bytes32 indexed rootClaim,
        uint256 l2BlockNumber,
        address parentRef
    );

    constructor(
        DomainConfig memory domainConfig_,
        IProofLaneVerifier[4] memory laneVerifiers_,
        IStakingRegistry stakingRegistry_,
        uint256 challengePeriod_,
        uint256 bondAmount_
    ) {
        if (challengePeriod_ == 0) revert InvalidChallengePeriod();
        if (address(stakingRegistry_) == address(0)) revert MissingStakingRegistry();

        for (uint256 i = 0; i < PROOF_LANE_COUNT; ++i) {
            if (address(laneVerifiers_[i]) == address(0)) revert MissingLaneVerifier(i);
            laneVerifiers[i] = laneVerifiers_[i];
        }

        domainConfig = domainConfig_;
        stakingRegistry = stakingRegistry_;
        challengePeriod = challengePeriod_;
        bondAmount = bondAmount_;
    }

    function propose(bytes32 rootClaim, uint256 l2BlockNumber, address parentRef, bytes32 intermediateRootsHash)
        external
        returns (ProofSystemGame game)
    {
        uint256 l1OriginNumber = block.number == 0 ? block.number : block.number - 1;
        RootCommitment memory commitment = RootCommitment({
            rootClaim: rootClaim,
            l2BlockNumber: l2BlockNumber,
            parentRef: parentRef,
            intermediateRootsHash: intermediateRootsHash,
            l1OriginHash: blockhash(l1OriginNumber),
            l1OriginNumber: l1OriginNumber
        });

        IProofLaneVerifier[4] memory verifiers;
        for (uint256 i = 0; i < PROOF_LANE_COUNT; ++i) {
            verifiers[i] = laneVerifiers[i];
        }

        game = new ProofSystemGame(domainConfig, commitment, verifiers, stakingRegistry, challengePeriod, bondAmount);

        games.push(address(game));

        emit ProofSystemGameCreated(address(game), game.rootId(), rootClaim, l2BlockNumber, parentRef);
    }

    function gameCount() external view returns (uint256) {
        return games.length;
    }

    function expectedRootId(RootCommitment calldata commitment) external view returns (bytes32) {
        return ProofRootIdLib.hashRootId(ProofRootIdLib.hashDomain(domainConfig), commitment);
    }
}
