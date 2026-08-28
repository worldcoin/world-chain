// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";
import {SafeCast} from "@openzeppelin/contracts/utils/math/SafeCast.sol";

import {GameTypes} from "../../src/dispute/lib/GameTypes.sol";
import {LibProof} from "../../src/dispute/lib/LibProof.sol";
import {MultiProofGame} from "../../src/dispute/MultiProofGame.sol";
import {ERC20StakingVault} from "../../src/dispute/ERC20StakingVault.sol";
import {IMultiProofGame} from "../../src/dispute/interfaces/IMultiProofGame.sol";
import {IERC20StakingVault} from "../../src/dispute/interfaces/IERC20StakingVault.sol";
import {IWorldChainProofVerifier} from "../../src/dispute/interfaces/IWorldChainProofVerifier.sol";

import {GameType} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";
import {IDisputeGameFactory} from "@optimism-bedrock/interfaces/dispute/IDisputeGameFactory.sol";
import {IAnchorStateRegistry} from "@optimism-bedrock/interfaces/dispute/IAnchorStateRegistry.sol";
import {IProxyAdmin} from "@optimism-bedrock/interfaces/universal/IProxyAdmin.sol";
import {ISystemConfig} from "@optimism-bedrock/interfaces/L1/ISystemConfig.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @notice Deploys the World Chain proof-system game implementation for the stock OP Stack dispute
///         infrastructure deployed by op-deployer.
///
/// Deploys the WIP-1006 singleton ERC-20 staking-vault proxy, or reuses the existing vault during
/// a game rotation, then deploys the `MultiProofGame` implementation. Registration and activation
/// are deliberately separate: run `ActivateProofSystem.s.sol` after reviewing the deployment
/// record and offchain inputs. This script never changes the factory or respected game type.
/// `setImplementation(WC_GAME_TYPE, address(0))` is the kill switch: it stops new game
/// creation without touching in-flight games.
/// When replacing type 1006, stop the proposer while implementation and bond registration are
/// updated. If the new implementation changes `domainHash`, restart the proposer so its selected
/// lineage uses the new domain; its bond manager will resolve ready proposer-owned games left in
/// the superseded domain. Same-domain games remain on the selected lineage and settle normally.
///
/// The three proof-lane verifiers are **inputs**, not outputs: this script never deploys them.
/// Every one is a required env address and must already hold code.
/// That is deliberate — a script that can mint its own verifiers can silently register a game
/// type that accepts any proof. Supply real contracts:
///
///   `VALIDITY_PROOF_VERIFIER`   — reusable `SP1ValidityVerifier`; a vkey change does not rotate it
///   `TEE_VERIFIER`              — e.g. `NitroProofVerifier` (see `DeployNitro.s.sol`)
///   `SECURITY_COUNCIL_VERIFIER` — council-controlled attestation verifier
/// For a local devnet, deploy the test doubles with `DeployProofMocks.s.sol` first and pass
/// its output addresses in. That keeps the choice to run against mocks explicit and auditable
/// at the call site instead of hidden inside this script.
///
/// Requires `just build-opstack` first: the 0.8.15 OP Proxy deploys from the
/// `opstack/out` artifacts via `deployCode`.
contract DeployProofSystem is Script {
    using SafeCast for uint256;

    struct Deployment {
        IDisputeGameFactory disputeGameFactory;
        IAnchorStateRegistry anchorStateRegistry;
        IProxyAdmin vaultProxyAdmin;
        IERC20StakingVault bondVault;
        MultiProofGame gameImpl;
    }

    struct Config {
        uint256 privateKey;
        uint256 l2ChainId;
        bytes32 rollupConfigHash;
        uint256 blockInterval;
        uint64 challengePeriod;
        uint64 proofPeriod;
        uint256 proposerBond;
        uint256 challengerBond;
        uint8 proofThreshold;
        address protocolFeeRecipient;
        bytes32 aggregationVKey;
        bytes32 rangeVKeyCommitment;
        bytes32 teeImageId;
        IWorldChainProofVerifier validityProofVerifier;
        IWorldChainProofVerifier teeVerifier;
        IWorldChainProofVerifier securityCouncil;
        IDisputeGameFactory disputeGameFactory;
        IAnchorStateRegistry anchorStateRegistry;
        ISystemConfig systemConfig;
        IProxyAdmin proxyAdmin;
        IERC20 bondToken;
        IERC20StakingVault existingBondVault;
        uint256 erc20WithdrawalDelay;
        uint256 proxyAdminOwnerKey;
    }

    uint64 internal constant DEFAULT_CHALLENGE_PERIOD = 1 days;
    uint64 internal constant DEFAULT_PROOF_PERIOD = 7 days;
    uint256 internal constant DEFAULT_BLOCK_INTERVAL = 450;
    uint256 internal constant TOKEN_UNIT = 1e18;
    uint256 internal constant DEFAULT_PROPOSER_BOND = TOKEN_UNIT / 100;
    uint256 internal constant DEFAULT_CHALLENGER_BOND = DEFAULT_PROPOSER_BOND;
    uint8 internal constant DEFAULT_PROOF_THRESHOLD = 2;

    function run() external returns (Deployment memory deployment) {
        Config memory config = _readConfig();
        _validateConfig(config);
        deployment.disputeGameFactory = config.disputeGameFactory;
        deployment.anchorStateRegistry = config.anchorStateRegistry;

        deployment.vaultProxyAdmin = config.proxyAdmin;
        if (address(config.existingBondVault) == address(0)) {
            vm.startBroadcast(config.privateKey);
            ERC20StakingVault vaultImpl = new ERC20StakingVault(config.erc20WithdrawalDelay);
            deployment.bondVault = IERC20StakingVault(
                deployCode("opstack/out/Proxy.sol/Proxy.json", abi.encode(address(config.proxyAdmin)))
            );
            vm.stopBroadcast();

            vm.startBroadcast(config.proxyAdminOwnerKey);
            config.proxyAdmin
                .upgradeAndCall(
                    payable(address(deployment.bondVault)),
                    address(vaultImpl),
                    abi.encodeCall(
                        IERC20StakingVault.initialize,
                        (config.bondToken, config.systemConfig, config.disputeGameFactory)
                    )
                );
            vm.stopBroadcast();
        } else {
            deployment.bondVault = config.existingBondVault;
        }

        // Verifiers are pre-existing inputs validated in `_validateConfig`.
        vm.startBroadcast(config.privateKey);
        deployment.gameImpl = new MultiProofGame(_gameConfig(deployment, config));
        vm.stopBroadcast();

        require(deployment.gameImpl.bondVault() == deployment.bondVault, "DeployProofSystem: vault wiring mismatch");

        _writeDeployment(deployment, config);
    }

    function _readConfig() internal view returns (Config memory config) {
        config.privateKey = vm.envUint("PRIVATE_KEY");
        config.l2ChainId = vm.envUint("WORLD_CHAIN_L2_CHAIN_ID");
        config.rollupConfigHash = vm.envBytes32("ROLLUP_CONFIG_HASH");
        config.blockInterval = vm.envOr("PROOF_SYSTEM_BLOCK_INTERVAL", DEFAULT_BLOCK_INTERVAL);
        config.challengePeriod = vm.envOr("CHALLENGE_PERIOD", uint256(DEFAULT_CHALLENGE_PERIOD)).toUint64();
        config.proofPeriod = vm.envOr("PROOF_PERIOD", uint256(DEFAULT_PROOF_PERIOD)).toUint64();
        config.proposerBond = vm.envOr("PROPOSER_BOND", DEFAULT_PROPOSER_BOND);
        config.challengerBond = vm.envOr("CHALLENGER_BOND", DEFAULT_CHALLENGER_BOND);
        config.proofThreshold = vm.envOr("PROOF_THRESHOLD", uint256(DEFAULT_PROOF_THRESHOLD)).toUint8();
        // Required: there is no sane default owner for proof-timeout forfeitures.
        config.protocolFeeRecipient = vm.envAddress("PROTOCOL_FEE_RECIPIENT");
        config.aggregationVKey = vm.envBytes32("AGGREGATION_VKEY");
        config.rangeVKeyCommitment = vm.envBytes32("RANGE_VKEY_COMMITMENT");
        config.teeImageId = vm.envBytes32("TEE_IMAGE_ID");
        // Proof lanes are required inputs, never deployed here. The SP1 address remains stable
        // when a new game implementation pins different vkeys.
        config.validityProofVerifier = IWorldChainProofVerifier(vm.envAddress("VALIDITY_PROOF_VERIFIER"));
        config.teeVerifier = IWorldChainProofVerifier(vm.envAddress("TEE_VERIFIER"));
        config.securityCouncil = IWorldChainProofVerifier(vm.envAddress("SECURITY_COUNCIL_VERIFIER"));
        config.disputeGameFactory = IDisputeGameFactory(vm.envAddress("DISPUTE_GAME_FACTORY"));
        config.anchorStateRegistry = IAnchorStateRegistry(vm.envAddress("ANCHOR_STATE_REGISTRY"));
        config.systemConfig = ISystemConfig(vm.envAddress("SYSTEM_CONFIG"));
        config.proxyAdmin = IProxyAdmin(vm.envAddress("OP_CHAIN_PROXY_ADMIN"));
        config.bondToken = IERC20(vm.envAddress("BOND_TOKEN"));
        config.existingBondVault = IERC20StakingVault(vm.envOr("ERC20_STAKING_VAULT", address(0)));
        config.erc20WithdrawalDelay = vm.envOr("ERC20_WITHDRAWAL_DELAY", uint256(300));
        config.proxyAdminOwnerKey = vm.envOr("OP_CHAIN_PROXY_ADMIN_OWNER_PRIVATE_KEY", uint256(0));
    }

    /// @dev A zero or codeless verifier would make the whole lane unverifiable, and
    ///      `MultiProofGame` only rejects the zero address — so check for code here, where the
    ///      failure is a clear deploy-time revert rather than a game that can never resolve.
    function _requireContract(address target, string memory label) internal view {
        require(target != address(0), string.concat("DeployProofSystem: ", label, " required"));
        require(target.code.length > 0, string.concat("DeployProofSystem: ", label, " has no code"));
    }

    function _validateConfig(Config memory config) internal view {
        _requireContract(address(config.validityProofVerifier), "VALIDITY_PROOF_VERIFIER");
        _requireContract(address(config.teeVerifier), "TEE_VERIFIER");
        _requireContract(address(config.securityCouncil), "SECURITY_COUNCIL_VERIFIER");
        _requireContract(address(config.bondToken), "BOND_TOKEN");

        // The 2-of-3 threshold only means anything if the lanes are independent: pointing two
        // lanes at one verifier lets a single party satisfy both and resolve on its own.
        require(
            address(config.validityProofVerifier) != address(config.teeVerifier)
                && address(config.validityProofVerifier) != address(config.securityCouncil)
                && address(config.teeVerifier) != address(config.securityCouncil),
            "DeployProofSystem: proof lane verifiers must be distinct"
        );

        require(
            address(config.anchorStateRegistry.disputeGameFactory()) == address(config.disputeGameFactory),
            "DeployProofSystem: ASR factory mismatch"
        );
        require(
            address(config.anchorStateRegistry.systemConfig()) == address(config.systemConfig),
            "DeployProofSystem: ASR SystemConfig mismatch"
        );
        require(config.systemConfig.l2ChainId() == config.l2ChainId, "DeployProofSystem: L2 chain ID mismatch");
        require(
            config.disputeGameFactory.owner() == config.proxyAdmin.owner(),
            "DeployProofSystem: DGF and ProxyAdmin owners must match"
        );
        IERC20StakingVault currentBondVault = _currentBondVault(config.disputeGameFactory);
        if (address(currentBondVault) != address(0)) {
            require(
                address(config.existingBondVault) != address(0), "DeployProofSystem: existing ERC-20 vault required"
            );
            require(config.existingBondVault == currentBondVault, "DeployProofSystem: must reuse current ERC-20 vault");
        }
        if (address(config.existingBondVault) == address(0)) {
            require(config.proxyAdminOwnerKey != 0, "DeployProofSystem: ProxyAdmin owner key required");
            require(
                vm.addr(config.proxyAdminOwnerKey) == config.proxyAdmin.owner(),
                "DeployProofSystem: ProxyAdmin owner key mismatch"
            );
        } else {
            _requireContract(address(config.existingBondVault), "ERC20_STAKING_VAULT");
            require(config.existingBondVault.token() == config.bondToken, "DeployProofSystem: vault token mismatch");
            require(
                config.existingBondVault.systemConfig() == config.systemConfig,
                "DeployProofSystem: vault SystemConfig mismatch"
            );
            require(
                config.existingBondVault.disputeGameFactory() == config.disputeGameFactory,
                "DeployProofSystem: vault factory mismatch"
            );
            require(
                config.existingBondVault.proxyAdmin() == config.proxyAdmin,
                "DeployProofSystem: vault ProxyAdmin mismatch"
            );
            require(
                config.existingBondVault.delay() == config.erc20WithdrawalDelay,
                "DeployProofSystem: vault withdrawal delay mismatch"
            );
        }
        require(config.protocolFeeRecipient != address(0), "DeployProofSystem: protocol fee recipient required");
        require(config.aggregationVKey != bytes32(0), "DeployProofSystem: aggregation vkey required");
        require(config.rangeVKeyCommitment != bytes32(0), "DeployProofSystem: range vkey required");
        require(config.teeImageId != bytes32(0), "DeployProofSystem: TEE image ID required");
        require(config.blockInterval > 0, "DeployProofSystem: block interval required");
        require(config.challengePeriod > 0, "DeployProofSystem: challenge period required");
        require(config.proofPeriod > config.challengePeriod, "DeployProofSystem: proof period must exceed challenge");
        require(config.proposerBond > 0, "DeployProofSystem: proposer bond required");
        require(config.challengerBond > 0, "DeployProofSystem: challenger bond required");
    }

    function _currentBondVault(IDisputeGameFactory factory) internal view returns (IERC20StakingVault bondVault) {
        IDisputeGame currentImplementation = factory.gameImpls(GameTypes.MULTI_PROOF_GAME_TYPE);
        if (address(currentImplementation) == address(0)) return IERC20StakingVault(address(0));

        try IMultiProofGame(address(currentImplementation)).bondVault() returns (IERC20StakingVault currentBondVault) {
            bondVault = currentBondVault;
        } catch {
            // The first ERC-20 bond migration may replace a legacy implementation without this getter.
        }
    }

    function _gameConfig(Deployment memory deployment, Config memory config)
        internal
        pure
        returns (IMultiProofGame.GameConfig memory)
    {
        return IMultiProofGame.GameConfig({
            rollupConfigHash: config.rollupConfigHash,
            blockInterval: config.blockInterval,
            challengePeriod: config.challengePeriod,
            proofPeriod: config.proofPeriod,
            proposerBond: config.proposerBond,
            challengerBond: config.challengerBond,
            protocolFeeRecipient: config.protocolFeeRecipient,
            proofThreshold: config.proofThreshold,
            aggregationVKey: config.aggregationVKey,
            rangeVKeyCommitment: config.rangeVKeyCommitment,
            teeImageId: config.teeImageId,
            validityProofVerifier: config.validityProofVerifier,
            teeVerifier: config.teeVerifier,
            securityCouncil: config.securityCouncil,
            anchorStateRegistry: config.anchorStateRegistry,
            bondVault: deployment.bondVault
        });
    }

    function _writeDeployment(Deployment memory deployment, Config memory config) internal {
        string memory out = vm.envOr("PROOF_SYSTEM_DEPLOYMENT_OUT", string(""));
        if (bytes(out).length == 0) return;

        string memory root = "deployment";
        // Legacy key names retained for offchain consumers: the "factory" is now the stock
        // DisputeGameFactory and the registry is the stock AnchorStateRegistry.
        vm.serializeAddress(root, "proofSystemFactory", address(deployment.disputeGameFactory));
        vm.serializeAddress(root, "disputeGameFactory", address(deployment.disputeGameFactory));
        vm.serializeAddress(root, "anchorStateRegistry", address(deployment.anchorStateRegistry));
        vm.serializeAddress(root, "systemConfig", address(config.systemConfig));
        vm.serializeAddress(root, "opChainProxyAdmin", address(config.proxyAdmin));
        vm.serializeAddress(root, "validityProofVerifier", address(config.validityProofVerifier));
        vm.serializeAddress(root, "teeVerifier", address(config.teeVerifier));
        vm.serializeAddress(root, "securityCouncil", address(config.securityCouncil));
        vm.serializeAddress(root, "protocolFeeRecipient", config.protocolFeeRecipient);
        vm.serializeAddress(root, "gameImplementation", address(deployment.gameImpl));
        vm.serializeAddress(root, "bondToken", address(config.bondToken));
        vm.serializeAddress(root, "erc20StakingVault", address(deployment.bondVault));
        vm.serializeAddress(root, "erc20StakingVaultProxyAdmin", address(deployment.vaultProxyAdmin));
        vm.serializeUint(root, "gameType", uint256(GameType.unwrap(GameTypes.MULTI_PROOF_GAME_TYPE)));
        vm.serializeBytes32(root, "rollupConfigHash", config.rollupConfigHash);
        vm.serializeBytes32(root, "aggregationVKey", config.aggregationVKey);
        vm.serializeBytes32(root, "rangeVKeyCommitment", config.rangeVKeyCommitment);
        vm.serializeBytes32(root, "teeImageId", config.teeImageId);
        vm.serializeUint(root, "l2ChainId", config.l2ChainId);
        vm.serializeUint(root, "proofSystemVersion", LibProof.PROOF_SYSTEM_VERSION);
        vm.serializeUint(root, "blockInterval", config.blockInterval);
        vm.serializeUint(root, "challengePeriod", config.challengePeriod);
        vm.serializeUint(root, "proofPeriod", config.proofPeriod);
        vm.serializeUint(root, "proposerBond", config.proposerBond);
        vm.serializeUint(root, "challengerBond", config.challengerBond);
        vm.serializeUint(root, "erc20WithdrawalDelay", config.erc20WithdrawalDelay);
        vm.serializeUint(root, "retirementTimestampAtDeployment", config.anchorStateRegistry.retirementTimestamp());
        vm.serializeAddress(root, "anchorGameAtDeployment", address(config.anchorStateRegistry.anchorGame()));
        string memory json = vm.serializeUint(root, "proofThreshold", config.proofThreshold);
        vm.writeJson(json, out);
    }
}
