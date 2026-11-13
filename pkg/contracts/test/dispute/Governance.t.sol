// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Governance, CheckGovernance} from "../../scripts/devnet/Governance.s.sol";
import {DeployCouncilSafe} from "../../scripts/devnet/DeployCouncilSafe.s.sol";
import {DeployProofSystem} from "../../scripts/devnet/DeployProofSystem.s.sol";
import {ActivateProofSystem} from "../../scripts/devnet/ActivateProofSystem.s.sol";
import {SubmitSecurityCouncilProof} from "../../scripts/devnet/SubmitSecurityCouncilProof.s.sol";
import {OPStackFixtures} from "./OPStackFixtures.sol";
import {MultiProofGame} from "../../src/dispute/MultiProofGame.sol";
import {IMultiProofGame} from "../../src/dispute/interfaces/IMultiProofGame.sol";
import {SecurityCouncilVerifier} from "../../src/dispute/council/SecurityCouncilVerifier.sol";
import {ISystemConfig} from "@optimism-bedrock/interfaces/L1/ISystemConfig.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";
import {Bitmap} from "../../src/dispute/lib/LibProof.sol";
import {GameType} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {Safe} from "@safe-global/safe-contracts/contracts/Safe.sol";
import {Enum} from "@safe-global/safe-contracts/contracts/common/Enum.sol";

abstract contract TestGovernanceConfig is Governance {
    bool internal safeMode;
    address internal safeAddress;
    uint256 internal firstKey = 0xA11CE;
    uint256 internal secondKey = 0xB0B;
    string internal safeTxOut;

    function configure(bool mode, address account, uint256 first, uint256 second) public {
        safeMode = mode;
        safeAddress = account;
        firstKey = first;
        secondKey = second;
    }

    function setSafeTxOut(string memory path) public {
        safeTxOut = path;
    }

    // Process-wide environment mutations race between parallel Forge tests.
    function _safeTxOut() internal view virtual override returns (string memory) {
        return safeTxOut;
    }

    function _safeMode() internal view virtual override returns (bool) {
        return safeMode;
    }

    function _safeAddress() internal view virtual override returns (address) {
        return safeAddress;
    }

    function _signerKeys() internal view virtual override returns (uint256, uint256) {
        return (firstKey, secondKey);
    }

    function _relayerKey() internal pure virtual override returns (uint256) {
        return 0xCAFE;
    }
}

contract CouncilDeploymentHarness is DeployCouncilSafe, TestGovernanceConfig {
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

contract CouncilSubmissionHarness is SubmitSecurityCouncilProof, TestGovernanceConfig {
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

contract SafeDeploymentHarness is DeployProofSystem, TestGovernanceConfig {
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

    Config internal config;

    function setConfig(Config memory value) external {
        config = value;
    }

    function _readConfig() internal view virtual override returns (Config memory) {
        return config;
    }
}

contract SafeActivationHarness is ActivateProofSystem, TestGovernanceConfig {
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

    Config internal config;

    function setConfig(Config memory value) external {
        config = value;
    }

    function _readConfig() internal view virtual override returns (Config memory) {
        return config;
    }
}

contract GovernanceHarness is TestGovernanceConfig {
    function execute(uint256 key, address target, bytes calldata data) external {
        _executeGovernance(key, target, data);
    }

    function signatures(bytes32 digest) external view returns (bytes memory) {
        return _safeSignatures(digest);
    }

    function fail() external pure {
        revert("deliberate failure");
    }
}

contract GovernanceTest is OPStackFixtures {
    uint256 internal constant FIRST_KEY = 0xA11CE;
    uint256 internal constant SECOND_KEY = 0xB0B;
    uint256 internal constant RELAYER_KEY = 0xCAFE;
    GovernanceHarness internal executor;
    Safe internal safe;
    SecurityCouncilVerifier internal council;

    function setUp() public override {
        super.setUp();
        executor = new GovernanceHarness();
        vm.setEnv("COUNCIL_SIGNATURES", "");
        vm.setEnv("PRIVATE_KEY", vm.toString(RELAYER_KEY));
        vm.setEnv("COUNCIL_DEPLOYMENT_OUT", "");
        CouncilDeploymentHarness deployer = new CouncilDeploymentHarness();
        deployer.configure(true, address(0), FIRST_KEY, SECOND_KEY);
        DeployCouncilSafe.Deployment memory deployed = deployer.run();
        safe = deployed.councilSafe;
        council = deployed.verifier;
        executor.configure(true, address(safe), FIRST_KEY, SECOND_KEY);
        dgf.transferOwnership(address(safe));
        proxyAdmin.transferOwnership(address(safe));
        systemConfig.setGuardian(address(safe));
    }

    function test_safeSignsInOwnerOrderAndExecutes() public {
        executor.execute(
            0, address(dgf), abi.encodeWithSignature("setInitBond(uint32,uint256)", uint32(1), uint256(42))
        );
        assertEq(dgf.initBonds(GameType.wrap(1)), 42);
        assertEq(safe.nonce(), 1);
    }

    function test_safeExecutesWithReversedSignerConfiguration() public {
        executor.configure(true, address(safe), SECOND_KEY, FIRST_KEY);
        executor.execute(
            0, address(dgf), abi.encodeWithSignature("setInitBond(uint32,uint256)", uint32(1), uint256(42))
        );
        assertEq(dgf.initBonds(GameType.wrap(1)), 42);
    }

    function test_environmentSafeConfigurationAndCouncilCheck() public {
        vm.setEnv("GOVERNANCE_MODE", "safe");
        vm.setEnv("GOVERNANCE_SAFE", vm.toString(address(safe)));
        vm.setEnv("GOVERNANCE_SIGNER_1_PRIVATE_KEY", "");
        vm.setEnv("GOVERNANCE_SIGNER_2_PRIVATE_KEY", "");
        vm.setEnv("SECURITY_COUNCIL_VERIFIER", vm.toString(address(council)));
        CheckGovernance checker = new CheckGovernance();
        checker.run();
        vm.setEnv("GOVERNANCE_MODE", "invalid");
        vm.expectRevert("Governance: invalid mode");
        checker.run();
        vm.setEnv("GOVERNANCE_MODE", "eoa");
    }

    function test_preparesSafeCallsWithoutKeysOrExecution() public {
        executor.setSafeTxOut("cache/governance-test.safe.json");
        executor.configure(true, address(safe), 0, 0);
        bytes memory data = abi.encodeWithSignature("setInitBond(uint32,uint256)", uint32(1), uint256(42));
        uint256 beforeBond = dgf.initBonds(GameType.wrap(1));
        executor.execute(0, address(dgf), data);
        executor.execute(0, address(dgf), data);
        string memory json = vm.readFile("cache/governance-test.safe.json");
        assertEq(vm.parseJsonString(json, ".chainId"), vm.toString(block.chainid));
        assertEq(vm.parseJsonAddress(json, ".meta.createdFromSafeAddress"), address(safe));
        assertEq(vm.parseJsonAddress(json, ".transactions[0].to"), address(dgf));
        assertEq(vm.parseJsonBytes(json, ".transactions[1].data"), data);
        assertEq(dgf.initBonds(GameType.wrap(1)), beforeBond);
        assertEq(safe.nonce(), 0);
        executor.setSafeTxOut("");
    }

    function test_reusesCouncilSafeWithoutSignerKeys() public {
        CouncilDeploymentHarness deployer = new CouncilDeploymentHarness();
        deployer.configure(true, address(safe), 0, 0);
        DeployCouncilSafe.Deployment memory deployed = deployer.run();
        assertEq(address(deployed.councilSafe), address(safe));
        assertEq(deployed.verifier.council(), address(safe));
        assertEq(safe.nonce(), 0);
    }

    function test_safeVaultAndActivationApprovalCheckpoints() public {
        vm.prank(address(safe));
        dgf.setImplementation(WC_GAME_TYPE, IDisputeGame(address(0)));
        vm.prank(address(safe));
        asr.setRespectedGameType(GameType.wrap(1));
        SafeDeploymentHarness deployer = new SafeDeploymentHarness();
        deployer.configure(true, address(safe), 0, 0);
        DeployProofSystem.Config memory config = _deploymentConfig();
        deployer.setConfig(config);
        deployer.setSafeTxOut("cache/vault-test.safe.json");
        DeployProofSystem.Deployment memory deployed = deployer.run();
        assertEq(address(deployed.gameImpl), address(0));
        assertEq(safe.nonce(), 0);
        string memory vaultJson = vm.readFile("cache/vault-test.safe.json");
        deployer.setSafeTxOut("");
        executor.execute(
            0,
            vm.parseJsonAddress(vaultJson, ".transactions[0].to"),
            vm.parseJsonBytes(vaultJson, ".transactions[0].data")
        );
        config.existingBondVault = deployed.bondVault;
        deployer.setConfig(config);
        deployed = deployer.run();
        assertEq(address(deployed.bondVault.token()), address(bondToken));
        SafeActivationHarness activation = new SafeActivationHarness();
        activation.configure(true, address(safe), 0, 0);
        activation.setConfig(
            ActivateProofSystem.Config({
                guardianKey: 0,
                dgfOwnerKey: 0,
                disputeGameFactory: dgf,
                anchorStateRegistry: asr,
                systemConfig: ISystemConfig(address(systemConfig)),
                proxyAdmin: proxyAdmin,
                bondToken: bondToken,
                gameImplementation: IMultiProofGame(address(deployed.gameImpl)),
                requireFreshAnchor: true
            })
        );
        activation.setSafeTxOut("cache/activation-test.safe.json");
        activation.run();
        assertEq(address(dgf.gameImpls(WC_GAME_TYPE)), address(0));
        assertEq(asr.respectedGameType().raw(), 1);
        assertEq(safe.nonce(), 1);
        string memory activationJson = vm.readFile("cache/activation-test.safe.json");
        activation.setSafeTxOut("");
        for (uint256 i; i < 3; i++) {
            string memory prefix = string.concat(".transactions[", vm.toString(i), "]");
            executor.execute(
                0,
                vm.parseJsonAddress(activationJson, string.concat(prefix, ".to")),
                vm.parseJsonBytes(activationJson, string.concat(prefix, ".data"))
            );
        }
        assertEq(address(dgf.gameImpls(WC_GAME_TYPE)), address(deployed.gameImpl));
        assertEq(asr.respectedGameType().raw(), 1006);
        assertEq(safe.nonce(), 4);
    }

    function test_safeRejectsDuplicateSigners() public {
        executor.configure(true, address(safe), FIRST_KEY, FIRST_KEY);
        vm.expectRevert("Governance: two distinct keys required");
        executor.execute(0, address(dgf), hex"1234");
    }

    function test_safeRejectsNonOwnerSigner() public {
        executor.configure(true, address(safe), FIRST_KEY, RELAYER_KEY);
        vm.expectRevert("Governance: signer mismatch");
        executor.execute(0, address(dgf), hex"1234");
    }

    function test_safeRejectsThresholdChange() public {
        vm.prank(address(safe));
        safe.changeThreshold(1);
        vm.expectRevert("Governance: expected 2-of-2 Safe");
        executor.execute(0, address(dgf), hex"1234");
    }

    function test_safeRejectsModules() public {
        vm.prank(address(safe));
        safe.enableModule(vm.addr(RELAYER_KEY));
        vm.expectRevert("Governance: modules bypass threshold");
        executor.execute(0, address(dgf), hex"1234");
    }

    function test_safeRejectsSingleSignature() public {
        bytes memory data = abi.encodeWithSignature("setInitBond(uint32,uint256)", uint32(1), uint256(42));
        bytes32 digest =
            safe.getTransactionHash(address(dgf), 0, data, Enum.Operation.Call, 0, 0, 0, address(0), address(0), 0);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(FIRST_KEY, digest);
        vm.expectRevert("GS020");
        safe.execTransaction(
            address(dgf),
            0,
            data,
            Enum.Operation.Call,
            0,
            0,
            0,
            address(0),
            payable(address(0)),
            abi.encodePacked(r, s, v)
        );
    }

    function test_safeRejectsDirectDeployerAdministration() public {
        vm.prank(vm.addr(RELAYER_KEY));
        vm.expectRevert("Ownable: caller is not the owner");
        dgf.setInitBond(WC_GAME_TYPE, 42);
    }

    function test_safePropagatesExecutionFailure() public {
        vm.expectRevert("GS013");
        executor.execute(0, address(executor), abi.encodeCall(GovernanceHarness.fail, ()));
        assertEq(safe.nonce(), 0);
    }

    function test_eoaExecutionAndCouncilDefault() public {
        vm.setEnv("COUNCIL_OWNERS", vm.toString(vm.addr(FIRST_KEY)));
        vm.setEnv("COUNCIL_THRESHOLD", "1");
        executor.configure(false, address(0), FIRST_KEY, SECOND_KEY);
        DeployCouncilSafe.Deployment memory deployed = new CouncilDeploymentHarness().run();
        assertEq(deployed.councilSafe.getThreshold(), 1);
        assertEq(deployed.councilSafe.getOwners().length, 1);
        vm.prank(address(safe));
        dgf.transferOwnership(vm.addr(FIRST_KEY));
        executor.execute(
            FIRST_KEY, address(dgf), abi.encodeWithSignature("setInitBond(uint32,uint256)", uint32(1), uint256(42))
        );
        assertEq(dgf.initBonds(GameType.wrap(1)), 42);
    }

    function test_eoaPropagatesExecutionFailure() public {
        executor.configure(false, address(0), FIRST_KEY, SECOND_KEY);
        vm.expectRevert("deliberate failure");
        executor.execute(FIRST_KEY, address(executor), abi.encodeCall(GovernanceHarness.fail, ()));
    }

    function test_safeDeploysVaultAndActivates() public {
        // Clearing the fixture implementation makes this a first-vault deployment.
        vm.prank(address(safe));
        dgf.setImplementation(WC_GAME_TYPE, IDisputeGame(address(0)));
        SafeDeploymentHarness deployer = new SafeDeploymentHarness();
        deployer.configure(true, address(safe), FIRST_KEY, SECOND_KEY);
        deployer.setConfig(_deploymentConfig());
        DeployProofSystem.Deployment memory deployed = deployer.run();
        assertEq(address(deployed.bondVault.proxyAdmin()), address(proxyAdmin));
        assertEq(address(deployed.bondVault.disputeGameFactory()), address(dgf));
        assertEq(address(deployed.bondVault.systemConfig()), address(systemConfig));
        assertEq(address(deployed.bondVault.token()), address(bondToken));
        vm.prank(address(safe));
        asr.setRespectedGameType(GameType.wrap(1));
        uint64 retirement = asr.retirementTimestamp();
        SafeActivationHarness activation = new SafeActivationHarness();
        activation.configure(true, address(safe), FIRST_KEY, SECOND_KEY);
        activation.setConfig(
            ActivateProofSystem.Config({
                guardianKey: 0,
                dgfOwnerKey: 0,
                disputeGameFactory: dgf,
                anchorStateRegistry: asr,
                systemConfig: ISystemConfig(address(systemConfig)),
                proxyAdmin: proxyAdmin,
                bondToken: bondToken,
                gameImplementation: IMultiProofGame(address(deployed.gameImpl)),
                requireFreshAnchor: true
            })
        );
        activation.run();
        assertEq(address(dgf.gameImpls(WC_GAME_TYPE)), address(deployed.gameImpl));
        assertEq(dgf.initBonds(WC_GAME_TYPE), 0);
        assertEq(asr.respectedGameType().raw(), WC_GAME_TYPE.raw());
        assertEq(asr.retirementTimestamp(), retirement);
        assertEq(safe.nonce(), 4);
    }

    function test_sharedSafeSubmitsCouncilProof() public {
        IMultiProofGame.GameConfig memory config = _gameConfig();
        config.securityCouncil = council;
        gameImpl = new MultiProofGame(config);
        vm.prank(address(safe));
        dgf.setImplementation(WC_GAME_TYPE, gameImpl);
        MultiProofGame game = _proposeAtAnchor();
        vm.setEnv("GAME_ADDRESS", vm.toString(address(game)));
        CouncilSubmissionHarness submission = new CouncilSubmissionHarness();
        submission.configure(true, address(safe), FIRST_KEY, SECOND_KEY);
        bytes32 messageHash = council.attestationDigest(game.rootId());
        (bool success, bytes memory result) =
            address(safe).staticcall(abi.encodeWithSignature("getMessageHash(bytes)", abi.encode(messageHash)));
        assertTrue(success);
        vm.setEnv("COUNCIL_SIGNATURES", vm.toString(executor.signatures(abi.decode(result, (bytes32)))));
        submission.configure(true, address(safe), 0, 0);
        submission.run();
        assertEq(Bitmap.unwrap(game.proofBitmap()), 4);
        vm.setEnv("COUNCIL_SIGNATURES", "");
    }

    function _deploymentConfig() internal view returns (DeployProofSystem.Config memory config) {
        config.privateKey = RELAYER_KEY;
        config.l2ChainId = CHAIN_ID;
        config.rollupConfigHash = ROLLUP_CONFIG_HASH;
        config.blockInterval = BLOCK_INTERVAL;
        config.challengePeriod = CHALLENGE_PERIOD;
        config.proofPeriod = PROOF_PERIOD;
        config.proposerBond = PROPOSER_BOND;
        config.challengerBond = CHALLENGER_BOND;
        config.proofThreshold = PROOF_THRESHOLD;
        config.protocolFeeRecipient = protocolFeeRecipient;
        config.aggregationVKey = AGGREGATION_VKEY;
        config.rangeVKeyCommitment = RANGE_VKEY_COMMITMENT;
        config.teeImageId = TEE_IMAGE_ID;
        config.validityProofVerifier = validityVerifier;
        config.teeVerifier = teeVerifier;
        config.securityCouncil = council;
        config.disputeGameFactory = dgf;
        config.anchorStateRegistry = asr;
        config.systemConfig = ISystemConfig(address(systemConfig));
        config.proxyAdmin = proxyAdmin;
        config.bondToken = bondToken;
        config.erc20WithdrawalDelay = ERC20_WITHDRAWAL_DELAY_SECONDS;
    }
}
