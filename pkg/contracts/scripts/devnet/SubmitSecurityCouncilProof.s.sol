// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Governance} from "./Governance.s.sol";

import {SecurityCouncilVerifier} from "../../src/dispute/council/SecurityCouncilVerifier.sol";
import {IMultiProofGame} from "../../src/dispute/interfaces/IMultiProofGame.sol";
import {LibProof, ProofLane} from "../../src/dispute/lib/LibProof.sol";

interface ICouncilSafe {
    function getMessageHash(bytes calldata message) external view returns (bytes32);
    function getThreshold() external view returns (uint256);
    function isOwner(address owner) external view returns (bool);
}

/// @notice Testing-only helper that signs and submits the Security Council lane for one game.
/// @dev EOA mode uses the existing 1-of-1 council; Safe mode uses the governance 2-of-2.
contract SubmitSecurityCouncilProof is Governance {
    function _firstSignerKey() internal view returns (uint256 key) {
        (key,) = _signerKeys();
    }

    function run() external {
        IMultiProofGame game = IMultiProofGame(vm.envAddress("GAME_ADDRESS"));
        uint256 signerKey = _safeMode() ? _firstSignerKey() : vm.envUint("COUNCIL_SIGNER_KEY");

        SecurityCouncilVerifier verifier = SecurityCouncilVerifier(address(game.securityCouncil()));
        ICouncilSafe council = ICouncilSafe(verifier.council());
        address signer = vm.addr(signerKey);

        if (_safeMode()) {
            require(address(council) == address(_governanceSafe()), "Council is not the governance Safe");
        } else {
            require(council.getThreshold() == 1, "Council threshold is not 1");
            require(council.isOwner(signer), "Signer is not a council owner");
        }

        bytes32 rootId = game.rootId();
        bytes32 attestationDigest = verifier.attestationDigest(rootId);
        // The Safe handler expects abi.encode(attestationDigest) wrapped as a SafeMessage.
        bytes32 safeMessageHash = council.getMessageHash(abi.encode(attestationDigest));
        bytes memory proof = _safeMode() ? _safeSignatures(safeMessageHash) : _sign(signerKey, safeMessageHash);

        require(verifier.verify(proof, bytes32(0), abi.encode(rootId)), "Council proof verification failed");

        uint256 transactionKey = vm.envOr("PRIVATE_KEY", signerKey);
        vm.startBroadcast(transactionKey);
        // Compact payload: lane id, reward recipient (`PROOF_RECIPIENT`, else the signer), proof.
        game.submitProofLane(
            abi.encodePacked(uint8(ProofLane.SECURITY_COUNCIL), vm.envOr("PROOF_RECIPIENT", signer), proof)
        );
        vm.stopBroadcast();
    }
}
