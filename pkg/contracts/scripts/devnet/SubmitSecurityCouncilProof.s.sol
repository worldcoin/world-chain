// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";

import {SecurityCouncilVerifier} from "../../src/proofs/council/SecurityCouncilVerifier.sol";
import {IMultiProofGame} from "../../src/proofs/interfaces/IMultiProofGame.sol";
import {LibProof} from "../../src/proofs/lib/LibProof.sol";

interface ICouncilSafe {
    function getMessageHash(bytes calldata message) external view returns (bytes32);
    function getThreshold() external view returns (uint256);
    function isOwner(address owner) external view returns (bool);
}

/// @notice Testing-only helper that signs and submits the Security Council lane for one game.
/// @dev Supports only a 1-of-1 council Safe; production councils require signature aggregation.
contract SubmitSecurityCouncilProof is Script {
    function run() external {
        IMultiProofGame game = IMultiProofGame(vm.envAddress("GAME_ADDRESS"));
        uint256 signerKey = vm.envUint("COUNCIL_SIGNER_KEY");

        SecurityCouncilVerifier verifier = SecurityCouncilVerifier(address(game.securityCouncil()));
        ICouncilSafe council = ICouncilSafe(verifier.council());
        address signer = vm.addr(signerKey);

        require(council.getThreshold() == 1, "Council threshold is not 1");
        require(council.isOwner(signer), "Signer is not a council owner");

        bytes32 rootId = game.rootId();
        bytes32 attestationDigest = verifier.attestationDigest(rootId);
        // The Safe handler expects abi.encode(attestationDigest) wrapped as a SafeMessage.
        bytes32 safeMessageHash = council.getMessageHash(abi.encode(attestationDigest));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(signerKey, safeMessageHash);
        bytes memory proof = abi.encodePacked(r, s, v);

        require(verifier.verify(rootId, proof), "Council proof verification failed");

        uint256 transactionKey = vm.envOr("PRIVATE_KEY", signerKey);
        vm.startBroadcast(transactionKey);
        game.submitProofLane(uint8(LibProof.ProofLane.SECURITY_COUNCIL), proof);
        vm.stopBroadcast();
    }
}
