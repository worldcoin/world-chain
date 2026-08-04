// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ActivateProofSystem} from "../../scripts/devnet/ActivateProofSystem.s.sol";
import {MultiProofGame} from "../../src/proofs/MultiProofGame.sol";
import {IMultiProofGame} from "../../src/proofs/interfaces/IMultiProofGame.sol";
import {OPStackFixtures} from "./OPStackFixtures.sol";

import {ISystemConfig} from "@optimism-bedrock/interfaces/L1/ISystemConfig.sol";

contract ActivateProofSystemHarness is ActivateProofSystem {
    function validate(Config memory config, IMultiProofGame implementation) external view {
        _validate(config, implementation);
    }
}

contract ActivateProofSystemTest is OPStackFixtures {
    uint256 internal constant GUARDIAN_KEY = 0xA11CE;

    ActivateProofSystemHarness internal activation;

    function setUp() public override {
        super.setUp();
        systemConfig.setGuardian(vm.addr(GUARDIAN_KEY));
        activation = new ActivateProofSystemHarness();
    }

    function test_validate_acceptsFreshBootstrap() public view {
        activation.validate(_activationConfig(true), IMultiProofGame(address(gameImpl)));
    }

    function test_validate_rejectsNonzeroAnchorForFreshBootstrap() public {
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);
        asr.setAnchorState(game);

        vm.expectRevert("ActivateProofSystem: fresh bootstrap has anchor game");
        activation.validate(_activationConfig(true), IMultiProofGame(address(gameImpl)));
    }

    function test_validate_rejectsRetiredAnchor() public {
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);
        asr.setAnchorState(game);

        vm.prank(vm.addr(GUARDIAN_KEY));
        asr.updateRetirementTimestamp();

        vm.expectRevert("ActivateProofSystem: anchor game retired");
        activation.validate(_activationConfig(false), IMultiProofGame(address(gameImpl)));
    }

    function _activationConfig(bool requireFreshAnchor) internal view returns (ActivateProofSystem.Config memory) {
        return ActivateProofSystem.Config({
            guardianKey: GUARDIAN_KEY,
            disputeGameFactory: dgf,
            anchorStateRegistry: asr,
            systemConfig: ISystemConfig(address(systemConfig)),
            requireFreshAnchor: requireFreshAnchor
        });
    }
}
