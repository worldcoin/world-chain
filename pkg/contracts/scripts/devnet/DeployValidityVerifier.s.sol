// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script, console} from "forge-std/Script.sol";

import {SP1ValidityVerifier} from "../../src/dispute/sp1/SP1ValidityVerifier.sol";
import {MultiProofGame} from "../../src/dispute/MultiProofGame.sol";
import {IWorldChainProofVerifier} from "../../src/dispute/interfaces/IWorldChainProofVerifier.sol";
import {ISP1Verifier} from "@sp1-contracts/src/ISP1Verifier.sol";

/// @notice Deploys a replacement `SP1ValidityVerifier` for the validity proof lane.
///
/// Both verification keys are immutable on the verifier, and the verifier address is immutable
/// on `MultiProofGame` — so re-keying the validity lane is a two-contract operation: deploy
/// here, then pass the result to `UpgradeGameImplementation.s.sol` as `VALIDITY_PROOF_VERIFIER`.
/// Registering a game whose verifier holds a stale `aggregationVKey` does not fail loudly; the
/// lane simply rejects every proof the workers produce, and the failure only surfaces as
/// proposals timing out a challenge window later.
///
/// `AGGREGATION_VKEY` and `RANGE_VKEY_COMMITMENT` must come from the `vkeys.json` built
/// alongside the ELF the SP1 workers are running — `just proof-deploy-validity-verifier` reads
/// them from that file rather than taking them by hand.
contract DeployValidityVerifier is Script {
    function run() external returns (SP1ValidityVerifier verifier) {
        uint256 privateKey = vm.envUint("PRIVATE_KEY");
        ISP1Verifier gateway = ISP1Verifier(vm.envAddress("SP1_VERIFIER_GATEWAY"));
        bytes32 aggregationVKey = vm.envBytes32("AGGREGATION_VKEY");
        bytes32 rangeVKeyCommitment = vm.envBytes32("RANGE_VKEY_COMMITMENT");

        require(address(gateway).code.length > 0, "DeployValidityVerifier: SP1 gateway has no code");

        // Deploying a verifier identical to the one already registered is almost always a
        // stale vkeys.json rather than an intended no-op, so refuse rather than register a
        // lane that will keep rejecting the workers' proofs.
        address currentGame = vm.envOr("CURRENT_GAME_IMPLEMENTATION", address(0));
        if (currentGame != address(0)) {
            SP1ValidityVerifier current =
                SP1ValidityVerifier(address(MultiProofGame(currentGame).validityProofVerifier()));
            require(
                current.aggregationVKey() != aggregationVKey || current.rangeVKeyCommitment() != rangeVKeyCommitment,
                "DeployValidityVerifier: vkeys already match the registered verifier"
            );
            console.log("  replacing verifier:      ", address(current));
            console.log("  old aggregationVKey:     ", vm.toString(current.aggregationVKey()));
            console.log("  old rangeVKeyCommitment: ", vm.toString(current.rangeVKeyCommitment()));
        }

        vm.startBroadcast(privateKey);
        verifier = new SP1ValidityVerifier(gateway, aggregationVKey, rangeVKeyCommitment);
        vm.stopBroadcast();

        console.log("SP1ValidityVerifier deployed");
        console.log("  address:                 ", address(verifier));
        console.log("  sp1Verifier gateway:     ", address(gateway));
        console.log("  aggregationVKey:         ", vm.toString(aggregationVKey));
        console.log("  rangeVKeyCommitment:     ", vm.toString(rangeVKeyCommitment));
    }
}
