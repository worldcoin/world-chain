// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";

import {WorldChainAnchorStateRegistry} from "../../src/proofs/WorldChainAnchorStateRegistry.sol";
import {WorldChainProofLib} from "../../src/proofs/WorldChainProofLib.sol";
import {WorldChainProofSystemGame} from "../../src/proofs/WorldChainProofSystemGame.sol";
import {WorldChainStakingRegistry} from "../../src/proofs/WorldChainStakingRegistry.sol";
import {IDisputeGame} from "../../src/proofs/external/IDisputeGame.sol";
import {IDisputeGameFactory} from "../../src/proofs/external/IDisputeGameFactory.sol";
import {IDelayedWETH} from "../../src/proofs/external/IDelayedWETH.sol";
import {GameType} from "../../src/proofs/external/Types.sol";
import {IWorldChainProofVerifier} from "../../src/proofs/verifiers/IWorldChainProofVerifier.sol";
import {MockDelayedWETH} from "../../src/proofs/mocks/MockDelayedWETH.sol";
import {MockRootIdVerifier} from "../../src/proofs/mocks/MockRootIdVerifier.sol";

/// @title DeployProofSystem
/// @notice Deploys the World Chain WIP-1006 proof system as an OP Stack
///         `IDisputeGame` game type against a real testnet OP deployment.
/// @dev Deploys the lane verifiers (mock stubs for now — lanes aren't built
///      yet), the World Chain staking registry, and the proof-system game
///      implementation, then registers the implementation and init bond on the
///      *deployed* `DisputeGameFactory` via `setImplementation`/`setInitBond`.
///      The game is impl-agnostic: swap the verifier addresses for the real ZK,
///      TEE, and Security Council verifiers without redeploying the game.
contract DeployProofSystem is Script {
    struct Config {
        uint256 privateKey;
        IDisputeGameFactory disputeGameFactory;
        IDelayedWETH delayedWeth;
        uint32 gameType;
        uint256 initBond;
        uint256 challengerBond;
        uint64 challengePeriod;
        uint64 proofPeriod;
        WorldChainProofLib.Domain domain;
        bytes32 startingRootClaim;
        uint256 startingL2BlockNumber;
    }

    struct Deployment {
        WorldChainAnchorStateRegistry anchor;
        WorldChainStakingRegistry staking;
        IWorldChainProofVerifier validityVerifier;
        IWorldChainProofVerifier teeVerifier;
        IWorldChainProofVerifier councilVerifier;
        IDelayedWETH delayedWeth;
        WorldChainProofSystemGame gameImpl;
    }

    function run() external returns (Deployment memory deployment) {
        Config memory cfg = _loadConfig();
        GameType gameType = GameType.wrap(cfg.gameType);

        vm.startBroadcast(cfg.privateKey);

        deployment.anchor =
            new WorldChainAnchorStateRegistry(cfg.startingRootClaim, cfg.startingL2BlockNumber, gameType);
        deployment.staking = new WorldChainStakingRegistry(cfg.challengerBond);

        // Stub lane verifiers for testnet-now: real interfaces, mock impls.
        deployment.validityVerifier = new MockRootIdVerifier(false);
        deployment.teeVerifier = new MockRootIdVerifier(false);
        deployment.councilVerifier = new MockRootIdVerifier(false);

        // Use a configured DelayedWETH if provided, otherwise deploy a mock with
        // zero delay for a devnet.
        deployment.delayedWeth =
            address(cfg.delayedWeth) == address(0) ? new MockDelayedWETH(0) : cfg.delayedWeth;

        deployment.gameImpl = new WorldChainProofSystemGame(
            gameType,
            cfg.domain,
            cfg.challengePeriod,
            cfg.proofPeriod,
            deployment.validityVerifier,
            deployment.teeVerifier,
            deployment.councilVerifier,
            deployment.staking,
            deployment.delayedWeth
        );

        // Register the implementation and init bond on the deployed OP factory.
        cfg.disputeGameFactory.setImplementation(gameType, IDisputeGame(address(deployment.gameImpl)));
        cfg.disputeGameFactory.setInitBond(gameType, cfg.initBond);

        vm.stopBroadcast();

        _writeDeployment(deployment, cfg);
    }

    function _loadConfig() internal view returns (Config memory cfg) {
        cfg.privateKey = vm.envUint("PRIVATE_KEY");
        cfg.disputeGameFactory = IDisputeGameFactory(vm.envAddress("DISPUTE_GAME_FACTORY"));
        cfg.delayedWeth = IDelayedWETH(vm.envOr("DELAYED_WETH", address(0)));
        cfg.gameType = uint32(vm.envUint("PROOF_SYSTEM_GAME_TYPE"));
        cfg.initBond = vm.envOr("PROPOSER_BOND", uint256(1 ether));
        cfg.challengerBond = vm.envOr("CHALLENGER_BOND", uint256(0.1 ether));
        cfg.challengePeriod = uint64(vm.envOr("CHALLENGE_PERIOD", uint256(1 days)));
        cfg.proofPeriod = uint64(vm.envOr("PROOF_PERIOD", uint256(7 days)));
        cfg.startingRootClaim = vm.envOr("STARTING_ROOT_CLAIM", bytes32(0));
        cfg.startingL2BlockNumber = vm.envOr("STARTING_L2_BLOCK_NUMBER", uint256(0));

        cfg.domain = WorldChainProofLib.Domain({
            chainId: vm.envUint("WORLD_CHAIN_L2_CHAIN_ID"),
            proofSystemVersion: vm.envOr("PROOF_SYSTEM_VERSION", uint256(1)),
            rollupConfigHash: vm.envBytes32("ROLLUP_CONFIG_HASH"),
            blockInterval: vm.envOr("PROOF_SYSTEM_BLOCK_INTERVAL", uint256(0)),
            intermediateBlockInterval: vm.envOr("PROOF_SYSTEM_INTERMEDIATE_BLOCK_INTERVAL", uint256(0))
        });
    }

    function _writeDeployment(Deployment memory deployment, Config memory cfg) internal {
        string memory out = vm.envOr("PROOF_SYSTEM_DEPLOYMENT_OUT", string(""));
        if (bytes(out).length == 0) return;

        string memory root = "deployment";
        vm.serializeAddress(root, "anchorStateRegistry", address(deployment.anchor));
        vm.serializeAddress(root, "stakingRegistry", address(deployment.staking));
        vm.serializeAddress(root, "validityProofVerifier", address(deployment.validityVerifier));
        vm.serializeAddress(root, "teeVerifier", address(deployment.teeVerifier));
        vm.serializeAddress(root, "securityCouncil", address(deployment.councilVerifier));
        vm.serializeAddress(root, "delayedWeth", address(deployment.delayedWeth));
        vm.serializeAddress(root, "gameImplementation", address(deployment.gameImpl));
        vm.serializeAddress(root, "disputeGameFactory", address(cfg.disputeGameFactory));
        vm.serializeUint(root, "gameType", cfg.gameType);
        vm.serializeUint(root, "l2ChainId", cfg.domain.chainId);
        string memory json = vm.serializeBytes32(root, "rollupConfigHash", cfg.domain.rollupConfigHash);
        vm.writeJson(json, out);
    }
}
