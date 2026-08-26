// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ActivateProofSystem} from "../../scripts/devnet/ActivateProofSystem.s.sol";
import {MultiProofGame} from "../../src/dispute/MultiProofGame.sol";
import {IMultiProofGame} from "../../src/dispute/interfaces/IMultiProofGame.sol";
import {OPStackFixtures} from "./OPStackFixtures.sol";

import {ISystemConfig} from "@optimism-bedrock/interfaces/L1/ISystemConfig.sol";

contract ActivateProofSystemHarness is ActivateProofSystem {
    function validate(Config memory config, IMultiProofGame implementation) external view {
        _validate(config, implementation);
    }
}

contract ActivateProofSystemTest is OPStackFixtures {
    uint256 internal constant GUARDIAN_KEY = 0xA11CE;
    uint256 internal constant DGF_OWNER_KEY = 0xB0B;

    ActivateProofSystemHarness internal activation;

    function setUp() public override {
        super.setUp();
        systemConfig.setGuardian(vm.addr(GUARDIAN_KEY));
        dgf.transferOwnership(vm.addr(DGF_OWNER_KEY));
        proxyAdmin.transferOwnership(vm.addr(DGF_OWNER_KEY));
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

    function test_validate_rejectsDivergedFactoryAndVaultOwners() public {
        vm.prank(vm.addr(DGF_OWNER_KEY));
        proxyAdmin.transferOwnership(makeAddr("new-proxy-admin-owner"));

        vm.expectRevert("ActivateProofSystem: DGF and ProxyAdmin owners must match");
        activation.validate(_activationConfig(false), IMultiProofGame(address(gameImpl)));
    }

    function test_validate_rejectsDuplicateProofLaneVerifiers() public {
        IMultiProofGame.GameConfig memory gameConfig = _gameConfig();
        gameConfig.teeVerifier = gameConfig.validityProofVerifier;
        MultiProofGame implementation = new MultiProofGame(gameConfig);

        vm.expectRevert("ActivateProofSystem: proof lane verifiers must be distinct");
        activation.validate(_activationConfig(false), IMultiProofGame(address(implementation)));
    }

    function test_run_registersValidatedImplementation() public {
        MultiProofGame implementation = new MultiProofGame(_gameConfig());
        vm.setEnv("DGF_OWNER_KEY", vm.toString(DGF_OWNER_KEY));
        vm.setEnv("DISPUTE_GAME_FACTORY", vm.toString(address(dgf)));
        vm.setEnv("ANCHOR_STATE_REGISTRY", vm.toString(address(asr)));
        vm.setEnv("SYSTEM_CONFIG", vm.toString(address(systemConfig)));
        vm.setEnv("OP_CHAIN_PROXY_ADMIN", vm.toString(address(proxyAdmin)));
        vm.setEnv("WLD_TOKEN", vm.toString(address(wld)));
        vm.setEnv("GAME_IMPLEMENTATION", vm.toString(address(implementation)));

        IMultiProofGame activated = activation.run();

        assertEq(address(activated), address(implementation));
        assertEq(address(dgf.gameImpls(WC_GAME_TYPE)), address(implementation));
        assertEq(dgf.initBonds(WC_GAME_TYPE), 0);
        assertEq(dgf.gameArgs(WC_GAME_TYPE), hex"");
    }

    function _activationConfig(bool requireFreshAnchor) internal view returns (ActivateProofSystem.Config memory) {
        return ActivateProofSystem.Config({
            guardianKey: GUARDIAN_KEY,
            dgfOwnerKey: DGF_OWNER_KEY,
            disputeGameFactory: dgf,
            anchorStateRegistry: asr,
            systemConfig: ISystemConfig(address(systemConfig)),
            proxyAdmin: proxyAdmin,
            wld: wld,
            gameImplementation: IMultiProofGame(address(gameImpl)),
            requireFreshAnchor: requireFreshAnchor
        });
    }
}
