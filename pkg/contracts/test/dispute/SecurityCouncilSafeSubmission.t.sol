// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {OPStackFixtures} from "./OPStackFixtures.sol";
import {MultiProofGame} from "../../src/dispute/MultiProofGame.sol";
import {SecurityCouncilVerifier} from "../../src/dispute/council/SecurityCouncilVerifier.sol";
import {IMultiProofGame} from "../../src/dispute/interfaces/IMultiProofGame.sol";
import {IWorldChainProofVerifier} from "../../src/dispute/interfaces/IWorldChainProofVerifier.sol";
import {Bitmap} from "../../src/dispute/lib/LibProof.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";
import {Safe} from "@safe-global/safe-contracts/contracts/Safe.sol";
import {
    CompatibilityFallbackHandler
} from "@safe-global/safe-contracts/contracts/handler/CompatibilityFallbackHandler.sol";
import {SafeProxyFactory} from "@safe-global/safe-contracts/contracts/proxies/SafeProxyFactory.sol";
import {SignMessageLib} from "@safe-global/safe-contracts/contracts/libraries/SignMessageLib.sol";
import {MultiSend} from "@safe-global/safe-contracts/contracts/libraries/MultiSend.sol";
import {Enum} from "@safe-global/safe-contracts/contracts/common/Enum.sol";

contract SecurityCouncilSafeSubmissionTest is OPStackFixtures {
    Safe internal safe;
    SecurityCouncilVerifier internal verifier;
    SignMessageLib internal signMessageLib;
    MultiSend internal multiSend;
    uint256 internal pk1;
    uint256 internal pk2;

    function setUp() public override {
        super.setUp();
        (pk1, pk2) = (0xA11CE, 0xB0B);
        if (vm.addr(pk1) > vm.addr(pk2)) (pk1, pk2) = (pk2, pk1);
        address[] memory owners = new address[](2);
        (owners[0], owners[1]) = (vm.addr(pk1), vm.addr(pk2));
        Safe singleton = new Safe();
        CompatibilityFallbackHandler handler = new CompatibilityFallbackHandler();
        safe = Safe(payable(address(new SafeProxyFactory().createProxyWithNonce(address(singleton), "", 0))));
        safe.setup(owners, 2, address(0), "", address(handler), address(0), 0, payable(address(0)));
        signMessageLib = new SignMessageLib();
        multiSend = new MultiSend();
        verifier = new SecurityCouncilVerifier(address(safe));
        IMultiProofGame.GameConfig memory config = _gameConfig();
        config.securityCouncil = IWorldChainProofVerifier(address(verifier));
        gameImpl = new MultiProofGame(config);
        dgf.setImplementation(WC_GAME_TYPE, IDisputeGame(address(gameImpl)), "");
    }

    function test_CouncilBatch_ApprovesAndSubmitsWithOneSafeTransaction() public {
        MultiProofGame game = _proposeAtAnchor();
        bytes memory data = _batch(game);
        assertTrue(_execute(data, _signatures(data, true)));
        assertEq(safe.nonce(), 1);
        assertEq(Bitmap.unwrap(game.proofBitmap()), 4);
        assertEq(game.laneRecipient(2), address(safe));
        assertTrue(verifier.verify("", bytes32(0), abi.encode(game.rootId())));
        assertFalse(verifier.verify("", bytes32(0), abi.encode(keccak256("another root"))));
    }

    function test_CouncilBatch_RequiresBothSafeOwners() public {
        MultiProofGame game = _proposeAtAnchor();
        bytes memory data = _batch(game);
        bytes memory sigs = _signatures(data, false);
        vm.expectRevert("GS020");
        _execute(data, sigs);
        assertFalse(verifier.verify("", bytes32(0), abi.encode(game.rootId())));
        assertEq(Bitmap.unwrap(game.proofBitmap()), 0);
    }

    function test_CouncilBatch_FailedSubmissionRollsBackApproval() public {
        MultiProofGame game = _proposeAtAnchor();
        bytes memory data = _batch(game);
        bytes memory sigs = _signatures(data, true);
        vm.warp(block.timestamp + PROOF_PERIOD + 1);
        vm.expectRevert("GS013");
        _execute(data, sigs);
        assertEq(safe.nonce(), 0);
        assertFalse(verifier.verify("", bytes32(0), abi.encode(game.rootId())));
        assertEq(Bitmap.unwrap(game.proofBitmap()), 0);
    }

    function test_CouncilBatch_SimulationDoesNotPersistApproval() public {
        MultiProofGame game = _proposeAtAnchor();
        (bool success,) =
            address(safe).call(abi.encodeWithSignature("simulate(address,bytes)", address(multiSend), _batch(game)));
        assertTrue(success);
        assertEq(safe.nonce(), 0);
        assertFalse(verifier.verify("", bytes32(0), abi.encode(game.rootId())));
        assertEq(Bitmap.unwrap(game.proofBitmap()), 0);
    }

    function _batch(MultiProofGame game) internal view returns (bytes memory) {
        bytes memory approval =
            abi.encodeCall(SignMessageLib.signMessage, (abi.encode(verifier.attestationDigest(game.rootId()))));
        bytes memory submission =
            abi.encodeCall(MultiProofGame.submitProofLane, (abi.encodePacked(uint8(2), address(safe))));
        bytes memory actions = abi.encodePacked(
            uint8(1),
            address(signMessageLib),
            uint256(0),
            approval.length,
            approval,
            uint8(0),
            address(game),
            uint256(0),
            submission.length,
            submission
        );
        return abi.encodeCall(MultiSend.multiSend, (actions));
    }

    function _signatures(bytes memory data, bool both) internal view returns (bytes memory) {
        bytes32 hash = safe.getTransactionHash(
            address(multiSend),
            0,
            data,
            Enum.Operation.DelegateCall,
            0,
            0,
            0,
            address(0),
            payable(address(0)),
            safe.nonce()
        );
        (uint8 v1, bytes32 r1, bytes32 s1) = vm.sign(pk1, hash);
        if (!both) return abi.encodePacked(r1, s1, v1);
        (uint8 v2, bytes32 r2, bytes32 s2) = vm.sign(pk2, hash);
        return abi.encodePacked(r1, s1, v1, r2, s2, v2);
    }

    function _execute(bytes memory data, bytes memory sigs) internal returns (bool) {
        return safe.execTransaction(
            address(multiSend), 0, data, Enum.Operation.DelegateCall, 0, 0, 0, address(0), payable(address(0)), sigs
        );
    }
}
