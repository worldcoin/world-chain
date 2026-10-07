// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {Governance} from "../../scripts/devnet/Governance.s.sol";
import {DeployNitro} from "../../scripts/devnet/DeployNitro.s.sol";
import {TransferNitroOwnership} from "../../scripts/devnet/TransferNitroOwnership.s.sol";
import {DeployCouncilSafe} from "../../scripts/devnet/DeployCouncilSafe.s.sol";
import {TestGovernanceConfig, CouncilDeploymentHarness, GovernanceHarness} from "./Governance.t.sol";
import {CertManager} from "@nitro-validator/CertManager.sol";
import {NitroAttestationVerifier} from "../../src/dispute/nitro/NitroAttestationVerifier.sol";
import {Safe} from "@safe-global/safe-contracts/contracts/Safe.sol";

contract NitroDeploymentHarness is DeployNitro, TestGovernanceConfig {
    function _safeTxOut() internal view override(Governance, TestGovernanceConfig) returns (string memory) {
        return safeTxOut;
    }

    function _safeMode() internal view override(Governance, TestGovernanceConfig) returns (bool) {
        return safeMode;
    }

    function _safeAddress() internal view override(Governance, TestGovernanceConfig) returns (address) {
        return safeAddress;
    }

    function _signerKeys() internal view override(Governance, TestGovernanceConfig) returns (uint256, uint256) {
        return (firstKey, secondKey);
    }

    function _relayerKey() internal pure override(Governance, TestGovernanceConfig) returns (uint256) {
        return 0xCAFE;
    }
}

contract NitroHandoffHarness is TransferNitroOwnership, TestGovernanceConfig {
    function _safeTxOut() internal view override(Governance, TestGovernanceConfig) returns (string memory) {
        return safeTxOut;
    }

    Config internal config;

    function setConfig(Config memory value) external {
        config = value;
    }

    function _readConfig() internal view override returns (Config memory) {
        return config;
    }

    function _safeMode() internal view override(Governance, TestGovernanceConfig) returns (bool) {
        return safeMode;
    }

    function _safeAddress() internal view override(Governance, TestGovernanceConfig) returns (address) {
        return safeAddress;
    }

    function _signerKeys() internal view override(Governance, TestGovernanceConfig) returns (uint256, uint256) {
        return (firstKey, secondKey);
    }

    function _relayerKey() internal pure override(Governance, TestGovernanceConfig) returns (uint256) {
        return 0xCAFE;
    }
}

contract NitroGovernanceTest is Test {
    uint256 internal constant ADMIN_KEY = 0xABCD;
    Safe internal safe;
    NitroDeploymentHarness internal deployer;
    NitroHandoffHarness internal handoff;
    GovernanceHarness internal executor;

    function setUp() public {
        vm.setEnv("PRIVATE_KEY", vm.toString(uint256(0xCAFE)));
        vm.setEnv("OWNER", vm.toString(vm.addr(ADMIN_KEY)));
        vm.setEnv("NITRO_DEPLOYMENT_OUT", "");
        vm.setEnv("COUNCIL_DEPLOYMENT_OUT", "");
        CouncilDeploymentHarness council = new CouncilDeploymentHarness();
        council.configure(true, address(0), 0xA11CE, 0xB0B);
        DeployCouncilSafe.Deployment memory deployed = council.run();
        safe = deployed.councilSafe;
        deployer = new NitroDeploymentHarness();
        handoff = new NitroHandoffHarness();
        executor = new GovernanceHarness();
        executor.configure(true, address(safe), 0xA11CE, 0xB0B);
    }

    function test_safeOwnsNitroAndApprovesPCRs() public {
        deployer.configure(true, address(safe), 0xA11CE, 0xB0B);
        DeployNitro.Deployment memory deployed = deployer.run();
        _assertOwner(deployed, address(safe));
        executor.execute(
            0,
            address(deployed.verifier),
            abi.encodeCall(
                NitroAttestationVerifier.approvePCRSet, (bytes32(uint256(1)), bytes32(uint256(2)), bytes32(uint256(3)))
            )
        );
        assertEq(safe.nonce(), 1);
        vm.prank(vm.addr(ADMIN_KEY));
        vm.expectRevert();
        deployed.verifier.approvePCRSet(bytes32(uint256(1)), bytes32(uint256(2)), bytes32(uint256(3)));
        executor.execute(0, address(deployed.certManager), abi.encodeCall(CertManager.setRevoker, (address(safe))));
        assertEq(deployed.certManager.revoker(), address(safe));
        assertTrue(deployed.verifier.isPCRSetApproved(bytes32(uint256(1)), bytes32(uint256(2)), bytes32(uint256(3))));
    }

    function test_eoaOwnsNitroAndApprovesPCRs() public {
        DeployNitro.Deployment memory deployed = deployer.run();
        _assertOwner(deployed, vm.addr(ADMIN_KEY));
        executor.configure(false, address(0), 0xA11CE, 0xB0B);
        executor.execute(
            ADMIN_KEY,
            address(deployed.verifier),
            abi.encodeCall(
                NitroAttestationVerifier.approvePCRSet, (bytes32(uint256(1)), bytes32(uint256(2)), bytes32(uint256(3)))
            )
        );
    }

    function test_preparesPCRApprovalWithoutExecuting() public {
        deployer.configure(true, address(safe), 0, 0);
        DeployNitro.Deployment memory deployed = deployer.run();
        executor.configure(true, address(safe), 0, 0);
        executor.setSafeTxOut("cache/nitro-test.safe.json");
        executor.execute(
            0,
            address(deployed.verifier),
            abi.encodeCall(
                NitroAttestationVerifier.approvePCRSet, (bytes32(uint256(1)), bytes32(uint256(2)), bytes32(uint256(3)))
            )
        );
        assertFalse(deployed.verifier.isPCRSetApproved(bytes32(uint256(1)), bytes32(uint256(2)), bytes32(uint256(3))));
        assertEq(safe.nonce(), 0);
        executor.setSafeTxOut("");
    }

    function test_handoffBothDirectionsAndRetry() public {
        DeployNitro.Deployment memory deployed = deployer.run();
        handoff.setConfig(
            TransferNitroOwnership.Config(
                address(safe), ADMIN_KEY, deployed.certManager, deployed.verifier, deployed.registry
            )
        );
        handoff.run();
        _assertOwner(deployed, address(safe));
        handoff.run();
        handoff.configure(true, address(safe), 0xA11CE, 0xB0B);
        handoff.setConfig(
            TransferNitroOwnership.Config(
                vm.addr(ADMIN_KEY), 0, deployed.certManager, deployed.verifier, deployed.registry
            )
        );
        handoff.run();
        _assertOwner(deployed, vm.addr(ADMIN_KEY));
    }

    function test_handoffResumesAfterCertManagerTransfer() public {
        DeployNitro.Deployment memory deployed = deployer.run();
        vm.startPrank(vm.addr(ADMIN_KEY));
        deployed.certManager.setRevoker(address(safe));
        deployed.certManager.transferOwnership(address(safe));
        vm.stopPrank();
        handoff.setConfig(
            TransferNitroOwnership.Config(
                address(safe), ADMIN_KEY, deployed.certManager, deployed.verifier, deployed.registry
            )
        );
        handoff.run();
        _assertOwner(deployed, address(safe));
    }

    function _assertOwner(DeployNitro.Deployment memory deployed, address owner) internal view {
        assertEq(deployed.certManager.owner(), owner);
        assertEq(deployed.certManager.revoker(), owner);
        assertEq(deployed.verifier.owner(), owner);
        assertEq(deployed.registry.owner(), owner);
    }
}
