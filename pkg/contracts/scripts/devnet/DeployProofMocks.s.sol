// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";

import {MockRootIdVerifier} from "../../test/mocks/MockRootIdVerifier.sol";

/// @notice Deploys the proof-lane test doubles for a local devnet and writes their addresses,
///         for `DeployProofSystem.s.sol` to consume via `VALIDITY_PROOF_VERIFIER`,
///         `TEE_VERIFIER`, and `SECURITY_COUNCIL_VERIFIER`.
///
/// Split out of `DeployProofSystem.s.sol` so that script cannot mint its own verifiers: a
/// deployer that fabricates its verifiers can silently register a game type that accepts any
/// proof. Running this script is the explicit, greppable opt-in to that behaviour.
///
/// @dev **Devnet only.** `MockRootIdVerifier(acceptAny=true)` accepts every proof. Never point a
///      chain whose withdrawals matter at these addresses.
contract DeployProofMocks is Script {
    struct Deployment {
        MockRootIdVerifier validityVerifier;
        MockRootIdVerifier teeVerifier;
        MockRootIdVerifier councilVerifier;
    }

    function run() external returns (Deployment memory deployment) {
        uint256 privateKey = vm.envUint("PRIVATE_KEY");
        vm.startBroadcast(privateKey);
        // Three separate instances, not one reused: `DeployProofSystem` requires the lanes to
        // be distinct so a single verifier cannot satisfy the multi-lane threshold alone.
        deployment.validityVerifier = new MockRootIdVerifier(true);
        deployment.teeVerifier = new MockRootIdVerifier(true);
        deployment.councilVerifier = new MockRootIdVerifier(true);
        vm.stopBroadcast();

        _writeDeployment(deployment);
    }

    function _writeDeployment(Deployment memory deployment) internal {
        string memory out = vm.envOr("PROOF_MOCKS_DEPLOYMENT_OUT", string(""));
        if (bytes(out).length == 0) return;

        string memory root = "mocks";
        vm.serializeAddress(root, "validityProofVerifier", address(deployment.validityVerifier));
        vm.serializeAddress(root, "teeVerifier", address(deployment.teeVerifier));
        string memory json = vm.serializeAddress(root, "securityCouncil", address(deployment.councilVerifier));
        vm.writeJson(json, out);
    }
}
