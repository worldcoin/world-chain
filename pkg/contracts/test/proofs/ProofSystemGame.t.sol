// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Test} from "forge-std/Test.sol";

import {IProofLaneVerifier} from "../../src/proofs/interfaces/IProofLaneVerifier.sol";
import {ProofSystemFactory} from "../../src/proofs/ProofSystemFactory.sol";
import {ProofSystemGame} from "../../src/proofs/ProofSystemGame.sol";
import {DomainConfig, ProofLane, RootState} from "../../src/proofs/ProofSystemTypes.sol";
import {MockProofLaneVerifier} from "./mocks/MockProofLaneVerifier.sol";
import {MockStakingRegistry} from "./mocks/MockStakingRegistry.sol";

contract ProofSystemGameTest is Test {
    uint256 internal constant CHALLENGE_PERIOD = 1 days;
    uint256 internal constant BOND_AMOUNT = 1 ether;

    address internal constant CHALLENGER = address(0xC411E);
    address internal constant PARENT_REF = address(0xA11CE);

    ProofSystemFactory internal factory;
    MockStakingRegistry internal stakingRegistry;
    MockProofLaneVerifier[4] internal laneVerifiers;

    function setUp() public {
        vm.roll(100);
        vm.warp(1_700_000_000);

        stakingRegistry = new MockStakingRegistry();

        IProofLaneVerifier[4] memory verifiers;
        for (uint256 i = 0; i < 4; ++i) {
            laneVerifiers[i] = new MockProofLaneVerifier();
            verifiers[i] = IProofLaneVerifier(address(laneVerifiers[i]));
        }

        DomainConfig memory domainConfig = DomainConfig({
            chainId: 480,
            proofSystemVersion: 1,
            rollupConfigHash: keccak256("world-chain-rollup-config"),
            blockInterval: 1_800,
            intermediateBlockInterval: 300
        });

        factory = new ProofSystemFactory(domainConfig, verifiers, stakingRegistry, CHALLENGE_PERIOD, BOND_AMOUNT);
    }

    function testUnchallengedRootFinalizesAfterChallengePeriod() public {
        ProofSystemGame game = _propose();

        vm.warp(block.timestamp + CHALLENGE_PERIOD);
        game.finalizeUnchallenged();

        assertEq(uint8(game.state()), uint8(RootState.FINALIZED));
    }

    function testUnstakedAccountCannotChallenge() public {
        ProofSystemGame game = _propose();

        vm.prank(CHALLENGER);
        vm.expectRevert(abi.encodeWithSelector(ProofSystemGame.ChallengerNotStaked.selector, CHALLENGER));
        game.challenge();
    }

    function testChallengeAtOrAfterDeadlineReverts() public {
        ProofSystemGame game = _propose();
        stakingRegistry.setStaked(CHALLENGER, true);

        vm.warp(block.timestamp + CHALLENGE_PERIOD);

        vm.prank(CHALLENGER);
        vm.expectRevert(ProofSystemGame.ChallengeDeadlineElapsed.selector);
        game.challenge();
    }

    function testStakedChallengeThenTwoDistinctLanesFinalize() public {
        ProofSystemGame game = _propose();
        bytes32 rootId = game.rootId();
        stakingRegistry.setStaked(CHALLENGER, true);

        vm.prank(CHALLENGER);
        game.challenge();

        laneVerifiers[uint8(ProofLane.VALIDITY_PROOF)].setSupportsRoot(rootId, true);
        laneVerifiers[uint8(ProofLane.TEE_ATTESTATION)].setSupportsRoot(rootId, true);

        game.submitProof(ProofLane.VALIDITY_PROOF, "validity-proof");
        assertEq(uint8(game.state()), uint8(RootState.CHALLENGED));
        assertEq(game.proofCount(), 1);

        game.submitProof(ProofLane.TEE_ATTESTATION, "tee-attestation");

        assertEq(uint8(game.state()), uint8(RootState.FINALIZED));
        assertEq(game.proofCount(), 2);
        assertEq(game.proofBitmap(), 0x05);
        assertEq(stakingRegistry.forfeitedBonds(rootId, CHALLENGER), BOND_AMOUNT);
    }

    function testDuplicateLaneDoesNotIncreaseThreshold() public {
        ProofSystemGame game = _challengedGame();
        bytes32 rootId = game.rootId();
        laneVerifiers[uint8(ProofLane.VALIDITY_PROOF)].setSupportsRoot(rootId, true);

        game.submitProof(ProofLane.VALIDITY_PROOF, "validity-proof");

        vm.expectRevert(abi.encodeWithSelector(ProofSystemGame.DuplicateProofLane.selector, ProofLane.VALIDITY_PROOF));
        game.submitProof(ProofLane.VALIDITY_PROOF, "another-validity-proof");

        assertEq(uint8(game.state()), uint8(RootState.CHALLENGED));
        assertEq(game.proofCount(), 1);
    }

    function testLaneRejectsProofForDifferentRootId() public {
        ProofSystemGame game = _challengedGame();
        bytes32 differentRootId = keccak256("different-root-id");
        laneVerifiers[uint8(ProofLane.VALIDITY_PROOF)].setSupportsRoot(differentRootId, true);

        vm.expectRevert(
            abi.encodeWithSelector(ProofSystemGame.InvalidProof.selector, ProofLane.VALIDITY_PROOF, game.rootId())
        );
        game.submitProof(ProofLane.VALIDITY_PROOF, "validity-proof");

        assertEq(uint8(game.state()), uint8(RootState.CHALLENGED));
        assertEq(game.proofCount(), 0);
    }

    function testInvalidityPreventsFinalization() public {
        ProofSystemGame game = _challengedGame();
        bytes32 rootId = game.rootId();

        laneVerifiers[uint8(ProofLane.FAULT_PROOF_GAME)].setInvalidatesRoot(rootId, true);
        game.submitInvalidity(ProofLane.FAULT_PROOF_GAME, "fault-game-result");

        assertEq(uint8(game.state()), uint8(RootState.INVALIDATED));
        assertEq(stakingRegistry.refundedBonds(rootId, CHALLENGER), BOND_AMOUNT);

        laneVerifiers[uint8(ProofLane.VALIDITY_PROOF)].setSupportsRoot(rootId, true);

        vm.expectRevert(abi.encodeWithSelector(ProofSystemGame.InvalidState.selector, RootState.INVALIDATED));
        game.submitProof(ProofLane.VALIDITY_PROOF, "validity-proof");
    }

    function _challengedGame() internal returns (ProofSystemGame game) {
        game = _propose();
        stakingRegistry.setStaked(CHALLENGER, true);

        vm.prank(CHALLENGER);
        game.challenge();

        assertEq(uint8(game.state()), uint8(RootState.CHALLENGED));
    }

    function _propose() internal returns (ProofSystemGame) {
        return factory.propose({
            rootClaim: keccak256("output-root"),
            l2BlockNumber: 1_800,
            parentRef: PARENT_REF,
            intermediateRootsHash: keccak256("intermediate-roots")
        });
    }
}
