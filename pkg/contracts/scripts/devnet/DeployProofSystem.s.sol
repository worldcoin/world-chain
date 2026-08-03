// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";

import {GameTypes} from "../../src/proofs/GameTypes.sol";
import {ProofLib} from "../../src/proofs/lib/ProofLib.sol";
import {MultiProofGame} from "../../src/proofs/MultiProofGame.sol";
import {IMultiProofGame} from "../../src/proofs/interfaces/IMultiProofGame.sol";
import {IWorldChainProofVerifier} from "../../src/proofs/interfaces/IWorldChainProofVerifier.sol";
import {IWorldChainStakingRegistry} from "../../src/proofs/interfaces/IWorldChainStakingRegistry.sol";

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
/// `DisputeGameFactory` (`setImplementation` + `setInitBond`) and optionally flips the
/// `AnchorStateRegistry`'s respected game type — the withdrawal cutover switch.
/// `setImplementation(WC_GAME_TYPE, address(0))` is the kill switch: it stops new game
/// creation without touching in-flight games.
///
/// The three proof-lane verifiers and the staking registry are **inputs**, not outputs: this
/// script never deploys them. Every one is a required env address and must already hold code.
/// That is deliberate — a script that can mint its own verifiers can silently register a game
/// type that accepts any proof. Supply real contracts:
///
///   `VALIDITY_PROOF_VERIFIER`   — e.g. `SP1ValidityVerifier`
///   `TEE_VERIFIER`              — e.g. `NitroProofVerifier` (see `DeployNitro.s.sol`)
///   `SECURITY_COUNCIL_VERIFIER` — council-controlled attestation verifier
///   `STAKING_REGISTRY`          — `IWorldChainStakingRegistry` implementation
///
/// For a local devnet, deploy the test doubles with `DeployProofMocks.s.sol` first and pass
/// its output addresses in. That keeps the choice to run against mocks explicit and auditable
/// at the call site instead of hidden inside this script.
///
/// Requires `just build-opstack` first: the 0.8.15 OP implementations (DelayedWETH,
/// Proxy, ProxyAdmin) deploy from the `opstack/out` artifacts via `deployCode`.
contract DeployProofSystem is Script {
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
        uint8 proofThreshold;
        address protocolFeeRecipient;
        uint256 challengeFee;
        IWorldChainProofVerifier validityProofVerifier;
        IWorldChainProofVerifier teeVerifier;
        IWorldChainProofVerifier securityCouncil;
        IWorldChainStakingRegistry stakingRegistry;
        IDisputeGameFactory disputeGameFactory;
        IAnchorStateRegistry anchorStateRegistry;
        ISystemConfig systemConfig;
        IProxyAdmin proxyAdmin;
        uint256 delayedWethDelay;
        uint256 proxyAdminOwnerKey;
        uint256 dgfOwnerKey;
        uint256 guardianKey;
        bool setRespectedGameType;
    }

    uint64 internal constant CHALLENGE_PERIOD = 1 days;
    uint64 internal constant PROOF_PERIOD = 7 days;
    uint256 internal constant PROPOSER_BOND = 1 ether;
    uint256 internal constant CHALLENGER_BOND = 0.1 ether;
    uint256 internal constant DEFAULT_CHALLENGE_FEE = 0.01 ether;

    function run() external returns (Deployment memory deployment) {
        Config memory config = _readConfig();
        _validateConfig(config);
        deployment.disputeGameFactory = config.disputeGameFactory;
        deployment.anchorStateRegistry = config.anchorStateRegistry;

        // 1. DelayedWETH + game implementation, from the deployer key. Verifiers and the
        // staking registry are pre-existing inputs validated in `_validateConfig`.
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
        config.disputeGameFactory.setInitBond(GameTypes.MULTI_PROOF_GAME_TYPE, PROPOSER_BOND);
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

        // 3. Optional cutover: make the WC game type the respected game type (guardian).
        if (config.setRespectedGameType) {
            vm.startBroadcast(config.guardianKey);
            config.anchorStateRegistry.setRespectedGameType(GameTypes.MULTI_PROOF_GAME_TYPE);
            vm.stopBroadcast();
            require(
                config.anchorStateRegistry.respectedGameType().raw() == GameTypes.MULTI_PROOF_GAME_TYPE.raw(),
                "DeployProofSystem: respected game type not activated"
            );
        }

        _writeDeployment(deployment, config);
    }

    function _readConfig() internal view returns (Config memory config) {
        config.privateKey = vm.envUint("PRIVATE_KEY");
        config.l2ChainId = vm.envUint("WORLD_CHAIN_L2_CHAIN_ID");
        config.rollupConfigHash = vm.envBytes32("ROLLUP_CONFIG_HASH");
        config.blockInterval = vm.envOr("PROOF_SYSTEM_BLOCK_INTERVAL", uint256(10));
        config.proofThreshold = uint8(vm.envOr("PROOF_THRESHOLD", uint256(ProofLib.PROOF_THRESHOLD)));
        // Required: there is no sane default owner for challenge-fee proceeds.
        config.protocolFeeRecipient = vm.envAddress("PROTOCOL_FEE_RECIPIENT");
        config.challengeFee = vm.envOr("CHALLENGE_FEE", DEFAULT_CHALLENGE_FEE);
        // Proof lanes and staking: required inputs, never deployed here.
        config.validityProofVerifier = IWorldChainProofVerifier(vm.envAddress("VALIDITY_PROOF_VERIFIER"));
        config.teeVerifier = IWorldChainProofVerifier(vm.envAddress("TEE_VERIFIER"));
        config.securityCouncil = IWorldChainProofVerifier(vm.envAddress("SECURITY_COUNCIL_VERIFIER"));
        config.stakingRegistry = IWorldChainStakingRegistry(vm.envAddress("STAKING_REGISTRY"));
        config.disputeGameFactory = IDisputeGameFactory(vm.envAddress("DISPUTE_GAME_FACTORY"));
        config.anchorStateRegistry = IAnchorStateRegistry(vm.envAddress("ANCHOR_STATE_REGISTRY"));
        config.systemConfig = ISystemConfig(vm.envAddress("SYSTEM_CONFIG"));
        config.proxyAdmin = IProxyAdmin(vm.envAddress("OP_CHAIN_PROXY_ADMIN"));
        config.delayedWethDelay = vm.envOr("DELAYED_WETH_DELAY", uint256(300));
        config.proxyAdminOwnerKey = vm.envUint("OP_CHAIN_PROXY_ADMIN_OWNER_PRIVATE_KEY");
        config.dgfOwnerKey = vm.envUint("DGF_OWNER_KEY");
        config.guardianKey = vm.envUint("GUARDIAN_KEY");
        config.setRespectedGameType = vm.envOr("SET_RESPECTED_GAME_TYPE", false);
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
        _requireContract(address(config.stakingRegistry), "STAKING_REGISTRY");

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
        require(config.protocolFeeRecipient != address(0), "DeployProofSystem: protocol fee recipient required");
        require(config.challengeFee > 0, "DeployProofSystem: challenge fee required");
        require(config.challengeFee <= CHALLENGER_BOND, "DeployProofSystem: challenge fee exceeds bond");
        require(config.challengeFee < PROPOSER_BOND, "DeployProofSystem: proposer bond must exceed challenge fee");
        if (config.setRespectedGameType) {
            require(config.guardianKey != 0, "DeployProofSystem: guardian key required for cutover");
        }
    }

    function _gameConfig(Deployment memory deployment, Config memory config)
        internal
        pure
        returns (IMultiProofGame.GameConfig memory)
    {
        return IMultiProofGame.GameConfig({
            domain: ProofLib.Domain({
                chainId: config.l2ChainId,
                proofSystemVersion: 1,
                rollupConfigHash: config.rollupConfigHash,
                blockInterval: config.blockInterval
            }),
            challengePeriod: CHALLENGE_PERIOD,
            proofPeriod: PROOF_PERIOD,
            proposerBond: PROPOSER_BOND,
            challengerBond: CHALLENGER_BOND,
            protocolFeeRecipient: config.protocolFeeRecipient,
            challengeFee: config.challengeFee,
            proofThreshold: config.proofThreshold,
            validityProofVerifier: config.validityProofVerifier,
            teeVerifier: config.teeVerifier,
            securityCouncil: config.securityCouncil,
            stakingRegistry: config.stakingRegistry,
            disputeGameFactory: config.disputeGameFactory,
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
        vm.serializeAddress(root, "anchorStateRegistry", address(deployment.anchorStateRegistry));
        vm.serializeAddress(root, "validityProofVerifier", address(config.validityProofVerifier));
        vm.serializeAddress(root, "teeVerifier", address(config.teeVerifier));
        vm.serializeAddress(root, "securityCouncil", address(config.securityCouncil));
        vm.serializeAddress(root, "stakingRegistry", address(config.stakingRegistry));
        vm.serializeAddress(root, "protocolFeeRecipient", config.protocolFeeRecipient);
        vm.serializeAddress(root, "gameImplementation", address(deployment.gameImpl));
        vm.serializeAddress(root, "delayedWeth", address(deployment.weth));
        vm.serializeAddress(root, "delayedWethProxyAdmin", address(deployment.wethProxyAdmin));
        vm.serializeUint(root, "gameType", uint256(GameType.unwrap(GameTypes.MULTI_PROOF_GAME_TYPE)));
        vm.serializeBytes32(root, "rollupConfigHash", config.rollupConfigHash);
        vm.serializeUint(root, "l2ChainId", config.l2ChainId);
        vm.serializeUint(root, "proofSystemVersion", 1);
        vm.serializeUint(root, "blockInterval", config.blockInterval);
        vm.serializeUint(root, "challengeFee", config.challengeFee);
        string memory json = vm.serializeUint(root, "proofThreshold", config.proofThreshold);
        vm.writeJson(json, out);
    }
}
