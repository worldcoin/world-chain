// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

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
/// Admin calls broadcast only when the corresponding key is provided (devnet); otherwise the
/// script logs the target and calldata for out-of-band (multisig) execution.
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
        GameType gameType;
        uint256 delayedWethDelay;
        uint256 dgfOwnerKey; // 0 = log calldata instead of broadcasting
        uint256 guardianKey; // 0 = log calldata instead of broadcasting
        bool setRespectedGameType;
    }

    uint64 internal constant CHALLENGE_PERIOD = 1 days;
    uint64 internal constant PROOF_PERIOD = 7 days;
    uint256 internal constant PROPOSER_BOND = 1 ether;
    uint256 internal constant CHALLENGER_BOND = 0.1 ether;

    function run() external returns (Deployment memory deployment) {
        Config memory config = _readConfig();
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

        // Dedicated DelayedWETH proxy for the WC game type. On devnet the ProxyAdmin is owned
        // by the deployer; in production the chain's canonical ProxyAdmin should administer it
        // so that bond claw-back authority (hold/recover) sits with the L1 ProxyAdmin owner.
        deployment.wethProxyAdmin = IProxyAdmin(
            deployCode("opstack/out/ProxyAdmin.sol/ProxyAdmin.json", abi.encode(vm.addr(config.privateKey)))
        );
        address wethImpl =
            deployCode("opstack/out/DelayedWETH.sol/DelayedWETH.json", abi.encode(config.delayedWethDelay));
        deployment.weth = IDelayedWETH(
            payable(deployCode("opstack/out/Proxy.sol/Proxy.json", abi.encode(address(deployment.wethProxyAdmin))))
        );
        deployment.wethProxyAdmin
            .upgradeAndCall(
                payable(address(deployment.weth)),
                wethImpl,
                abi.encodeCall(IDelayedWETH.initialize, (config.systemConfig))
            );

        deployment.gameImpl = new WorldChainProofSystemGame(_gameConfig(deployment, config));
        vm.stopBroadcast();

        // 2. Register the game type on the existing DisputeGameFactory (factory owner).
        // Explicit signature: setImplementation is overloaded (2- and 3-arg forms).
        bytes memory setImplCall = abi.encodeWithSignature(
            "setImplementation(uint32,address)", GameType.unwrap(config.gameType), address(deployment.gameImpl)
        );
        bytes memory setBondCall = abi.encodeCall(IDisputeGameFactory.setInitBond, (config.gameType, PROPOSER_BOND));
        if (config.dgfOwnerKey != 0) {
            vm.startBroadcast(config.dgfOwnerKey);
            config.disputeGameFactory.setImplementation(config.gameType, IDisputeGame(address(deployment.gameImpl)));
            config.disputeGameFactory.setInitBond(config.gameType, PROPOSER_BOND);
            vm.stopBroadcast();

            require(
                address(config.disputeGameFactory.gameImpls(config.gameType)) == address(deployment.gameImpl),
                "DeployProofSystem: game implementation not registered"
            );
            require(
                config.disputeGameFactory.initBonds(config.gameType) == deployment.gameImpl.proposerBond(),
                "DeployProofSystem: init bond does not match proposer bond"
            );
        } else {
            console2.log("DGF owner actions required (execute from factory owner):");
            console2.log("  target:", address(config.disputeGameFactory));
            console2.logBytes(setImplCall);
            console2.logBytes(setBondCall);
        }

        // 3. Optional cutover: make the WC game type the respected game type (guardian).
        if (config.setRespectedGameType) {
            bytes memory respectCall = abi.encodeCall(IAnchorStateRegistry.setRespectedGameType, (config.gameType));
            if (config.guardianKey != 0) {
                vm.startBroadcast(config.guardianKey);
                config.anchorStateRegistry.setRespectedGameType(config.gameType);
                vm.stopBroadcast();
            } else {
                console2.log("Guardian action required (execute from guardian):");
                console2.log("  target:", address(config.anchorStateRegistry));
                console2.logBytes(respectCall);
            }
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
        config.gameType = GameType.wrap(uint32(vm.envOr("WC_GAME_TYPE", uint256(42))));
        config.delayedWethDelay = vm.envOr("DELAYED_WETH_DELAY", uint256(300));
        config.dgfOwnerKey = vm.envOr("DGF_OWNER_KEY", uint256(0));
        config.guardianKey = vm.envOr("GUARDIAN_KEY", uint256(0));
        config.setRespectedGameType = vm.envOr("SET_RESPECTED_GAME_TYPE", false);
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
            gameType: config.gameType,
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
        vm.serializeUint(root, "gameType", uint256(GameType.unwrap(config.gameType)));
        vm.serializeBytes32(root, "rollupConfigHash", config.rollupConfigHash);
        vm.serializeUint(root, "l2ChainId", config.l2ChainId);
        vm.serializeUint(root, "proofSystemVersion", 1);
        vm.serializeUint(root, "blockInterval", config.blockInterval);
        string memory json = vm.serializeUint(root, "proofThreshold", config.proofThreshold);
        vm.writeJson(json, out);
    }
}
