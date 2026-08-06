// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";
import {SafeCast} from "@openzeppelin/contracts/utils/math/SafeCast.sol";

import {GameTypes} from "../../src/dispute/GameTypes.sol";
import {LibProof} from "../../src/dispute/lib/LibProof.sol";
import {MultiProofGame} from "../../src/dispute/MultiProofGame.sol";
import {IMultiProofGame} from "../../src/dispute/interfaces/IMultiProofGame.sol";
import {IWorldChainProofVerifier} from "../../src/dispute/interfaces/IWorldChainProofVerifier.sol";

import {GameType} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";
import {IDisputeGameFactory} from "@optimism-bedrock/interfaces/dispute/IDisputeGameFactory.sol";
import {IAnchorStateRegistry} from "@optimism-bedrock/interfaces/dispute/IAnchorStateRegistry.sol";
import {IDelayedWETH} from "@optimism-bedrock/interfaces/dispute/IDelayedWETH.sol";
import {IProxyAdmin} from "@optimism-bedrock/interfaces/universal/IProxyAdmin.sol";
import {ISystemConfig} from "@optimism-bedrock/interfaces/L1/ISystemConfig.sol";

/// @notice Registers the World Chain proof-system game on the stock OP Stack dispute
///         infrastructure deployed by op-deployer.
///
/// Deploys a DelayedWETH proxy dedicated to the World Chain game type and the
/// `MultiProofGame` implementation, then registers that implementation on the existing
/// `DisputeGameFactory` (`setImplementation` + `setInitBond`). Activation is deliberately
/// separate: run `ActivateProofSystem.s.sol` after deployment records and offchain inputs are
/// ready. This script never changes the respected game type or retirement timestamp.
/// `setImplementation(WC_GAME_TYPE, address(0))` is the kill switch: it stops new game
/// creation without touching in-flight games.
///
/// The three proof-lane verifiers are **inputs**, not outputs: this script never deploys them.
/// Every one is a required env address and must already hold code.
/// That is deliberate — a script that can mint its own verifiers can silently register a game
/// type that accepts any proof. Supply real contracts:
///
///   `VALIDITY_PROOF_VERIFIER`   — e.g. `SP1ValidityVerifier`
///   `TEE_VERIFIER`              — e.g. `NitroProofVerifier` (see `DeployNitro.s.sol`)
///   `SECURITY_COUNCIL_VERIFIER` — council-controlled attestation verifier
/// For a local devnet, deploy the test doubles with `DeployProofMocks.s.sol` first and pass
/// its output addresses in. That keeps the choice to run against mocks explicit and auditable
/// at the call site instead of hidden inside this script.
///
/// Requires `just build-opstack` first: the 0.8.15 OP implementations (DelayedWETH,
/// Proxy, ProxyAdmin) deploy from the `opstack/out` artifacts via `deployCode`.
contract DeployProofSystem is Script {
    using SafeCast for uint256;

    struct Deployment {
        IDisputeGameFactory disputeGameFactory;
        IAnchorStateRegistry anchorStateRegistry;
        IProxyAdmin wethProxyAdmin;
        IDelayedWETH weth;
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
        IWorldChainProofVerifier validityProofVerifier;
        IWorldChainProofVerifier teeVerifier;
        IWorldChainProofVerifier securityCouncil;
        IDisputeGameFactory disputeGameFactory;
        IAnchorStateRegistry anchorStateRegistry;
        ISystemConfig systemConfig;
        IProxyAdmin proxyAdmin;
        uint256 delayedWethDelay;
        uint256 proxyAdminOwnerKey;
        uint256 dgfOwnerKey;
    }

    uint64 internal constant DEFAULT_CHALLENGE_PERIOD = 1 days;
    uint64 internal constant DEFAULT_PROOF_PERIOD = 7 days;
    uint256 internal constant DEFAULT_BLOCK_INTERVAL = 450;
    uint256 internal constant DEFAULT_PROPOSER_BOND = 0.01 ether;
    uint256 internal constant DEFAULT_CHALLENGER_BOND = 0.001 ether;
    uint8 internal constant DEFAULT_PROOF_THRESHOLD = 2;

    function run() external returns (Deployment memory deployment) {
        Config memory config = _readConfig();
        _validateConfig(config);
        deployment.disputeGameFactory = config.disputeGameFactory;
        deployment.anchorStateRegistry = config.anchorStateRegistry;

        // 1. DelayedWETH + game implementation, from the deployer key. Verifiers are
        // pre-existing inputs validated in `_validateConfig`.
        vm.startBroadcast(config.privateKey);

        // The dedicated DelayedWETH proxy is administered by the chain's existing ProxyAdmin.
        deployment.wethProxyAdmin = config.proxyAdmin;
        address wethImpl =
            deployCode("opstack/out/DelayedWETH.sol/DelayedWETH.json", abi.encode(config.delayedWethDelay));
        deployment.weth = IDelayedWETH(
            payable(deployCode("opstack/out/Proxy.sol/Proxy.json", abi.encode(address(config.proxyAdmin))))
        );
        vm.stopBroadcast();

        vm.startBroadcast(config.proxyAdminOwnerKey);
        config.proxyAdmin
            .upgradeAndCall(
                payable(address(deployment.weth)),
                wethImpl,
                abi.encodeCall(IDelayedWETH.initialize, (config.systemConfig))
            );
        vm.stopBroadcast();

        vm.startBroadcast(config.privateKey);
        deployment.gameImpl = new MultiProofGame(_gameConfig(deployment, config));
        vm.stopBroadcast();

        // 2. Register the game type on the existing DisputeGameFactory (factory owner).
        // The three-argument overload explicitly clears any stale implementation args.
        vm.startBroadcast(config.dgfOwnerKey);
        config.disputeGameFactory
            .setImplementation(GameTypes.MULTI_PROOF_GAME_TYPE, IDisputeGame(address(deployment.gameImpl)), hex"");
        config.disputeGameFactory.setInitBond(GameTypes.MULTI_PROOF_GAME_TYPE, config.proposerBond);
        vm.stopBroadcast();

        require(
            address(config.disputeGameFactory.gameImpls(GameTypes.MULTI_PROOF_GAME_TYPE))
                == address(deployment.gameImpl),
            "DeployProofSystem: game implementation not registered"
        );
        require(
            config.disputeGameFactory.gameArgs(GameTypes.MULTI_PROOF_GAME_TYPE).length == 0,
            "DeployProofSystem: stale game implementation args"
        );
        require(
            config.disputeGameFactory.initBonds(GameTypes.MULTI_PROOF_GAME_TYPE) == deployment.gameImpl.proposerBond(),
            "DeployProofSystem: init bond does not match proposer bond"
        );

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
        // Proof lanes are required inputs, never deployed here.
        config.validityProofVerifier = IWorldChainProofVerifier(vm.envAddress("VALIDITY_PROOF_VERIFIER"));
        config.teeVerifier = IWorldChainProofVerifier(vm.envAddress("TEE_VERIFIER"));
        config.securityCouncil = IWorldChainProofVerifier(vm.envAddress("SECURITY_COUNCIL_VERIFIER"));
        config.disputeGameFactory = IDisputeGameFactory(vm.envAddress("DISPUTE_GAME_FACTORY"));
        config.anchorStateRegistry = IAnchorStateRegistry(vm.envAddress("ANCHOR_STATE_REGISTRY"));
        config.systemConfig = ISystemConfig(vm.envAddress("SYSTEM_CONFIG"));
        config.proxyAdmin = IProxyAdmin(vm.envAddress("OP_CHAIN_PROXY_ADMIN"));
        config.delayedWethDelay = vm.envOr("DELAYED_WETH_DELAY", uint256(300));
        config.proxyAdminOwnerKey = vm.envUint("OP_CHAIN_PROXY_ADMIN_OWNER_PRIVATE_KEY");
        config.dgfOwnerKey = vm.envUint("DGF_OWNER_KEY");
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
        require(config.dgfOwnerKey != 0, "DeployProofSystem: DGF owner key required");
        require(config.proxyAdminOwnerKey != 0, "DeployProofSystem: ProxyAdmin owner key required");
        require(
            vm.addr(config.dgfOwnerKey) == config.disputeGameFactory.owner(),
            "DeployProofSystem: DGF owner key mismatch"
        );
        require(
            vm.addr(config.proxyAdminOwnerKey) == config.proxyAdmin.owner(),
            "DeployProofSystem: ProxyAdmin owner key mismatch"
        );
        require(config.protocolFeeRecipient != address(0), "DeployProofSystem: protocol fee recipient required");
        require(config.blockInterval > 0, "DeployProofSystem: block interval required");
        require(config.challengePeriod > 0, "DeployProofSystem: challenge period required");
        require(config.proofPeriod > config.challengePeriod, "DeployProofSystem: proof period must exceed challenge");
        require(config.proposerBond > 0, "DeployProofSystem: proposer bond required");
        require(config.challengerBond > 0, "DeployProofSystem: challenger bond required");
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
            validityProofVerifier: config.validityProofVerifier,
            teeVerifier: config.teeVerifier,
            securityCouncil: config.securityCouncil,
            anchorStateRegistry: config.anchorStateRegistry,
            weth: deployment.weth
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
        vm.serializeAddress(root, "delayedWeth", address(deployment.weth));
        vm.serializeAddress(root, "delayedWethProxyAdmin", address(deployment.wethProxyAdmin));
        vm.serializeUint(root, "gameType", uint256(GameType.unwrap(GameTypes.MULTI_PROOF_GAME_TYPE)));
        vm.serializeBytes32(root, "rollupConfigHash", config.rollupConfigHash);
        vm.serializeUint(root, "l2ChainId", config.l2ChainId);
        vm.serializeUint(root, "proofSystemVersion", LibProof.PROOF_SYSTEM_VERSION);
        vm.serializeUint(root, "blockInterval", config.blockInterval);
        vm.serializeUint(root, "challengePeriod", config.challengePeriod);
        vm.serializeUint(root, "proofPeriod", config.proofPeriod);
        vm.serializeUint(root, "proposerBond", config.proposerBond);
        vm.serializeUint(root, "challengerBond", config.challengerBond);
        vm.serializeUint(root, "delayedWethDelay", config.delayedWethDelay);
        vm.serializeUint(root, "retirementTimestampAtDeployment", config.anchorStateRegistry.retirementTimestamp());
        vm.serializeAddress(root, "anchorGameAtDeployment", address(config.anchorStateRegistry.anchorGame()));
        string memory json = vm.serializeUint(root, "proofThreshold", config.proofThreshold);
        vm.writeJson(json, out);
    }
}
