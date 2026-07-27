// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";

import {WorldChainGameTypes} from "../../src/proofs/WorldChainGameTypes.sol";
import {WorldChainProofLib} from "../../src/proofs/WorldChainProofLib.sol";
import {WorldChainProofSystemGame} from "../../src/proofs/WorldChainProofSystemGame.sol";
import {IWorldChainProofVerifier} from "../../src/proofs/interfaces/IWorldChainProofVerifier.sol";
import {IWorldChainStakingRegistry} from "../../src/proofs/interfaces/IWorldChainStakingRegistry.sol";
import {MockRootIdVerifier} from "../../src/proofs/mocks/MockRootIdVerifier.sol";
import {MockStakingRegistry} from "../../src/proofs/mocks/MockStakingRegistry.sol";

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
/// Deploys: mock verifiers + staking registry (devnet), a DelayedWETH proxy dedicated to the
/// World Chain game type, and the `WorldChainProofSystemGame` implementation. Then registers
/// the implementation on the existing `DisputeGameFactory` (`setImplementation` + `setInitBond`)
/// and optionally flips the `AnchorStateRegistry`'s respected game type — the withdrawal
/// cutover switch. `setImplementation(WC_GAME_TYPE, address(0))` is the kill switch: it stops
/// new game creation without touching in-flight games.
///
/// Requires `just build-opstack` first: the 0.8.15 OP implementations (DelayedWETH,
/// Proxy, ProxyAdmin) deploy from the `opstack/out` artifacts via `deployCode`.
///
/// @dev This script is devnet-only: it deliberately deploys mock proof verifiers and must not
///      be used to activate a production withdrawal game type.
contract DeployProofSystem is Script {
    struct Deployment {
        IDisputeGameFactory disputeGameFactory;
        IAnchorStateRegistry anchorStateRegistry;
        MockRootIdVerifier validityVerifier;
        MockRootIdVerifier teeVerifier;
        MockRootIdVerifier councilVerifier;
        MockStakingRegistry staking;
        IProxyAdmin wethProxyAdmin;
        IDelayedWETH weth;
        WorldChainProofSystemGame gameImpl;
    }

    struct Config {
        uint256 privateKey;
        address challenger;
        uint256 l2ChainId;
        bytes32 rollupConfigHash;
        uint256 blockInterval;
        uint8 proofThreshold;
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

    function run() external returns (Deployment memory deployment) {
        Config memory config = _readConfig();
        _validateConfig(config);
        deployment.disputeGameFactory = config.disputeGameFactory;
        deployment.anchorStateRegistry = config.anchorStateRegistry;

        // 1. Periphery + DelayedWETH + game implementation, from the deployer key.
        vm.startBroadcast(config.privateKey);
        deployment.staking = new MockStakingRegistry();
        deployment.validityVerifier = new MockRootIdVerifier(false);
        deployment.teeVerifier = new MockRootIdVerifier(false);
        deployment.councilVerifier = new MockRootIdVerifier(false);
        deployment.staking.setStaked(vm.addr(config.privateKey), true);
        if (config.challenger != address(0)) {
            deployment.staking.setStaked(config.challenger, true);
        }

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
        deployment.gameImpl = new WorldChainProofSystemGame(_gameConfig(deployment, config));
        vm.stopBroadcast();

        // 2. Register the game type on the existing DisputeGameFactory (factory owner).
        // The three-argument overload explicitly clears any stale implementation args.
        vm.startBroadcast(config.dgfOwnerKey);
        config.disputeGameFactory
            .setImplementation(WorldChainGameTypes.WIP_1006, IDisputeGame(address(deployment.gameImpl)), hex"");
        config.disputeGameFactory.setInitBond(WorldChainGameTypes.WIP_1006, PROPOSER_BOND);
        vm.stopBroadcast();

        require(
            address(config.disputeGameFactory.gameImpls(WorldChainGameTypes.WIP_1006)) == address(deployment.gameImpl),
            "DeployProofSystem: game implementation not registered"
        );
        require(
            config.disputeGameFactory.gameArgs(WorldChainGameTypes.WIP_1006).length == 0,
            "DeployProofSystem: stale game implementation args"
        );
        require(
            config.disputeGameFactory.initBonds(WorldChainGameTypes.WIP_1006) == deployment.gameImpl.proposerBond(),
            "DeployProofSystem: init bond does not match proposer bond"
        );

        // 3. Optional cutover: make the WC game type the respected game type (guardian).
        if (config.setRespectedGameType) {
            vm.startBroadcast(config.guardianKey);
            config.anchorStateRegistry.setRespectedGameType(WorldChainGameTypes.WIP_1006);
            vm.stopBroadcast();
            require(
                config.anchorStateRegistry.respectedGameType().raw() == WorldChainGameTypes.WIP_1006.raw(),
                "DeployProofSystem: respected game type not activated"
            );
        }

        _writeDeployment(deployment, config);
    }

    function _readConfig() internal view returns (Config memory config) {
        config.privateKey = vm.envUint("PRIVATE_KEY");
        config.challenger = vm.envOr("WORLD_CHALLENGER_ADDRESS", address(0));
        config.l2ChainId = vm.envUint("WORLD_CHAIN_L2_CHAIN_ID");
        config.rollupConfigHash = vm.envBytes32("ROLLUP_CONFIG_HASH");
        config.blockInterval = vm.envOr("PROOF_SYSTEM_BLOCK_INTERVAL", uint256(10));
        config.proofThreshold = uint8(vm.envOr("PROOF_THRESHOLD", uint256(WorldChainProofLib.PROOF_THRESHOLD)));
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

    function _validateConfig(Config memory config) internal view {
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
        if (config.setRespectedGameType) {
            require(config.guardianKey != 0, "DeployProofSystem: guardian key required for cutover");
        }
    }

    function _gameConfig(Deployment memory deployment, Config memory config)
        internal
        pure
        returns (WorldChainProofSystemGame.GameConfig memory)
    {
        return WorldChainProofSystemGame.GameConfig({
            domain: WorldChainProofLib.Domain({
                chainId: config.l2ChainId,
                proofSystemVersion: 1,
                rollupConfigHash: config.rollupConfigHash,
                blockInterval: config.blockInterval
            }),
            challengePeriod: CHALLENGE_PERIOD,
            proofPeriod: PROOF_PERIOD,
            proposerBond: PROPOSER_BOND,
            challengerBond: CHALLENGER_BOND,
            proofThreshold: config.proofThreshold,
            validityProofVerifier: IWorldChainProofVerifier(address(deployment.validityVerifier)),
            teeVerifier: IWorldChainProofVerifier(address(deployment.teeVerifier)),
            securityCouncil: IWorldChainProofVerifier(address(deployment.councilVerifier)),
            stakingRegistry: IWorldChainStakingRegistry(address(deployment.staking)),
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
        vm.serializeAddress(root, "validityProofVerifier", address(deployment.validityVerifier));
        vm.serializeAddress(root, "teeVerifier", address(deployment.teeVerifier));
        vm.serializeAddress(root, "securityCouncil", address(deployment.councilVerifier));
        vm.serializeAddress(root, "stakingRegistry", address(deployment.staking));
        vm.serializeAddress(root, "gameImplementation", address(deployment.gameImpl));
        vm.serializeAddress(root, "delayedWeth", address(deployment.weth));
        vm.serializeAddress(root, "delayedWethProxyAdmin", address(deployment.wethProxyAdmin));
        vm.serializeUint(root, "gameType", uint256(GameType.unwrap(WorldChainGameTypes.WIP_1006)));
        vm.serializeBytes32(root, "rollupConfigHash", config.rollupConfigHash);
        vm.serializeUint(root, "l2ChainId", config.l2ChainId);
        vm.serializeUint(root, "proofSystemVersion", 1);
        vm.serializeUint(root, "blockInterval", config.blockInterval);
        string memory json = vm.serializeUint(root, "proofThreshold", config.proofThreshold);
        vm.writeJson(json, out);
    }
}
