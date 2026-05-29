// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {WorldChainProofLib} from "./WorldChainProofLib.sol";
import {WorldChainProofSystemGame} from "./WorldChainProofSystemGame.sol";
import {IWorldChainProofVerifier} from "./interfaces/IWorldChainProofVerifier.sol";
import {IWorldChainStakingRegistry} from "./interfaces/IWorldChainStakingRegistry.sol";

contract WorldChainProofSystemFactory {
    error GameAlreadyExists(bytes32 proposalKey, address game);
    error InvalidActivationParameters();

    event GameCreated(
        bytes32 indexed proposalKey,
        bytes32 indexed rootId,
        address indexed game,
        address proposer,
        bytes32 rootClaim,
        uint256 l2BlockNumber,
        address parentRef,
        bytes32 l1OriginHash,
        uint256 l1OriginNumber
    );

    struct GameCreatedLog {
        bytes32 proposalKey;
        bytes32 rootId;
        address game;
        address proposer;
        bytes32 rootClaim;
        uint256 l2BlockNumber;
        address parentRef;
        bytes32 l1OriginHash;
        uint256 l1OriginNumber;
    }

    WorldChainProofLib.Domain public domain;
    bytes32 public immutable domainHash;

    uint64 public immutable challengePeriod;
    uint64 public immutable proofPeriod;
    uint256 public immutable proposerBond;
    uint256 public immutable challengerBond;

    IWorldChainProofVerifier public immutable validityProofVerifier;
    IWorldChainProofVerifier public immutable teeVerifier;
    IWorldChainProofVerifier public immutable securityCouncil;
    IWorldChainStakingRegistry public immutable stakingRegistry;

    mapping(bytes32 proposalKey => address game) public games;

    constructor(
        WorldChainProofLib.Domain memory domain_,
        uint64 challengePeriod_,
        uint64 proofPeriod_,
        uint256 proposerBond_,
        uint256 challengerBond_,
        IWorldChainProofVerifier validityProofVerifier_,
        IWorldChainProofVerifier teeVerifier_,
        IWorldChainProofVerifier securityCouncil_,
        IWorldChainStakingRegistry stakingRegistry_
    ) {
        if (
            challengePeriod_ == 0 || proofPeriod_ == 0 || domain_.chainId == 0 || domain_.proofSystemVersion == 0
                || domain_.blockInterval == 0 || domain_.intermediateBlockInterval == 0
                || domain_.blockInterval % domain_.intermediateBlockInterval != 0
        ) {
            revert InvalidActivationParameters();
        }

        domain = domain_;
        domainHash = WorldChainProofLib.domainHash(domain_);
        challengePeriod = challengePeriod_;
        proofPeriod = proofPeriod_;
        proposerBond = proposerBond_;
        challengerBond = challengerBond_;
        validityProofVerifier = validityProofVerifier_;
        teeVerifier = teeVerifier_;
        securityCouncil = securityCouncil_;
        stakingRegistry = stakingRegistry_;
    }

    function propose(address parentRef, bytes32 rootClaim, uint256 l2BlockNumber, bytes32 intermediateRootsHash)
        external
        payable
        returns (address game, bytes32 id)
    {
        uint256 l1OriginNumber = block.number == 0 ? 0 : block.number - 1;
        bytes32 l1OriginHash = block.number == 0 ? bytes32(0) : blockhash(l1OriginNumber);

        bytes32 key =
            WorldChainProofLib.proposalKey(domainHash, parentRef, rootClaim, l2BlockNumber, intermediateRootsHash);
        address existing = games[key];
        if (existing != address(0)) revert GameAlreadyExists(key, existing);

        id = WorldChainProofLib.rootId(
            domainHash, parentRef, rootClaim, l2BlockNumber, intermediateRootsHash, l1OriginHash, l1OriginNumber
        );

        game = address(
            new WorldChainProofSystemGame{value: msg.value}(
                WorldChainProofSystemGame.ProposalInit({
                    proposer: msg.sender,
                    parentRef: parentRef,
                    rootClaim: rootClaim,
                    l2BlockNumber: l2BlockNumber,
                    intermediateRootsHash: intermediateRootsHash,
                    l1OriginHash: l1OriginHash,
                    l1OriginNumber: l1OriginNumber
                }),
                WorldChainProofSystemGame.ActivationConfig({
                    domainHash: domainHash,
                    challengePeriod: challengePeriod,
                    proofPeriod: proofPeriod,
                    proposerBond: proposerBond,
                    challengerBond: challengerBond,
                    validityProofVerifier: validityProofVerifier,
                    teeVerifier: teeVerifier,
                    securityCouncil: securityCouncil,
                    stakingRegistry: stakingRegistry
                })
            )
        );
        games[key] = game;

        _emitGameCreated(
            GameCreatedLog({
                proposalKey: key,
                rootId: id,
                game: game,
                proposer: msg.sender,
                rootClaim: rootClaim,
                l2BlockNumber: l2BlockNumber,
                parentRef: parentRef,
                l1OriginHash: l1OriginHash,
                l1OriginNumber: l1OriginNumber
            })
        );
    }

    function computeProposalKey(
        address parentRef,
        bytes32 rootClaim,
        uint256 l2BlockNumber,
        bytes32 intermediateRootsHash
    ) external view returns (bytes32) {
        return WorldChainProofLib.proposalKey(domainHash, parentRef, rootClaim, l2BlockNumber, intermediateRootsHash);
    }

    function computeRootId(
        address parentRef,
        bytes32 rootClaim,
        uint256 l2BlockNumber,
        bytes32 intermediateRootsHash,
        bytes32 l1OriginHash,
        uint256 l1OriginNumber
    ) external view returns (bytes32) {
        return WorldChainProofLib.rootId(
            domainHash, parentRef, rootClaim, l2BlockNumber, intermediateRootsHash, l1OriginHash, l1OriginNumber
        );
    }

    function _emitGameCreated(GameCreatedLog memory log) private {
        emit GameCreated(
            log.proposalKey,
            log.rootId,
            log.game,
            log.proposer,
            log.rootClaim,
            log.l2BlockNumber,
            log.parentRef,
            log.l1OriginHash,
            log.l1OriginNumber
        );
    }
}
