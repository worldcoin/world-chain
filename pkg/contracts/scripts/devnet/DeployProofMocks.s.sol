// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";

import {MockRootIdVerifier} from "../../src/proofs/mocks/MockRootIdVerifier.sol";
import {MockStakingRegistry} from "../../src/proofs/mocks/MockStakingRegistry.sol";

/// @notice Deploys the proof-lane test doubles for a local devnet and writes their addresses,
///         for `DeployProofSystem.s.sol` to consume via `VALIDITY_PROOF_VERIFIER`,
///         `TEE_VERIFIER`, `SECURITY_COUNCIL_VERIFIER` and `STAKING_REGISTRY`.
///
/// Split out of `DeployProofSystem.s.sol` so that script cannot mint its own verifiers: a
/// deployer that fabricates its verifiers can silently register a game type that accepts any
/// proof. Running this script is the explicit, greppable opt-in to that behaviour.
///
/// @dev **Devnet only.** `MockRootIdVerifier(acceptAny=true)` accepts every proof and
///      `MockStakingRegistry` has unauthenticated mutators. Never point a chain whose
///      withdrawals matter at these addresses.
contract DeployProofMocks is Script {
    struct Deployment {
        MockRootIdVerifier validityVerifier;
        MockRootIdVerifier teeVerifier;
        MockRootIdVerifier councilVerifier;
        MockStakingRegistry staking;
    }

    function run() external returns (Deployment memory deployment) {
        uint256 privateKey = vm.envUint("PRIVATE_KEY");
        // Staked so the devnet proposer/challenger can act without a separate step.
        address challenger = vm.envOr("WORLD_CHALLENGER_ADDRESS", address(0));

        vm.startBroadcast(privateKey);
        deployment.staking = new MockStakingRegistry();
        // Three separate instances, not one reused: `DeployProofSystem` requires the lanes to
        // be distinct so a single verifier cannot satisfy the multi-lane threshold alone.
        deployment.validityVerifier = new MockRootIdVerifier(true);
        deployment.teeVerifier = new MockRootIdVerifier(true);
        deployment.councilVerifier = new MockRootIdVerifier(true);
        deployment.staking.setStaked(vm.addr(privateKey), true);
        if (challenger != address(0)) {
            deployment.staking.setStaked(challenger, true);
        }
        vm.stopBroadcast();

        _writeDeployment(deployment);
    }

    function _writeDeployment(Deployment memory deployment) internal {
        string memory out = vm.envOr("PROOF_MOCKS_DEPLOYMENT_OUT", string(""));
        if (bytes(out).length == 0) return;

        string memory root = "mocks";
        vm.serializeAddress(root, "validityProofVerifier", address(deployment.validityVerifier));
        vm.serializeAddress(root, "teeVerifier", address(deployment.teeVerifier));
        vm.serializeAddress(root, "securityCouncil", address(deployment.councilVerifier));
        string memory json = vm.serializeAddress(root, "stakingRegistry", address(deployment.staking));
        vm.writeJson(json, out);
    }
}
