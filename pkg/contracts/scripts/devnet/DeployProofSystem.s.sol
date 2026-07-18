// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";

import {WorldChainAnchorStateRegistry} from "../../src/proofs/WorldChainAnchorStateRegistry.sol";
import {WorldChainProofLib} from "../../src/proofs/WorldChainProofLib.sol";
import {WorldChainProofSystemFactory} from "../../src/proofs/WorldChainProofSystemFactory.sol";
import {IWorldChainAnchorStateRegistry} from "../../src/proofs/interfaces/IWorldChainAnchorStateRegistry.sol";
import {MockRootIdVerifier} from "../../src/proofs/mocks/MockRootIdVerifier.sol";
import {MockStakingRegistry} from "../../src/proofs/mocks/MockStakingRegistry.sol";

contract DeployProofSystem is Script {
    struct Deployment {
        WorldChainAnchorStateRegistry anchor;
        MockRootIdVerifier validityVerifier;
        MockRootIdVerifier teeVerifier;
        MockRootIdVerifier councilVerifier;
        MockStakingRegistry staking;
        WorldChainProofSystemFactory factory;
    }

    struct Config {
        uint256 privateKey;
        address challenger;
        uint256 l2ChainId;
        bytes32 rollupConfigHash;
        uint256 blockInterval;
        uint8 proofThreshold;
    }

    uint64 internal constant CHALLENGE_PERIOD = 1 days;
    uint64 internal constant PROOF_PERIOD = 7 days;
    uint256 internal constant PROPOSER_BOND = 1 ether;
    uint256 internal constant CHALLENGER_BOND = 0.1 ether;

    function run() external returns (Deployment memory deployment) {
        Config memory config = _readConfig();

        vm.startBroadcast(config.privateKey);
        // Devnet advances anchors immediately after resolution; production deployments must configure a maturity delay.
        deployment.anchor = new WorldChainAnchorStateRegistry(bytes32(0), 0, 0);
        deployment.staking = new MockStakingRegistry();
        deployment.validityVerifier = new MockRootIdVerifier(false);
        deployment.teeVerifier = new MockRootIdVerifier(false);
        deployment.councilVerifier = new MockRootIdVerifier(false);
        deployment.staking.setStaked(vm.addr(config.privateKey), true);
        if (config.challenger != address(0)) {
            deployment.staking.setStaked(config.challenger, true);
        }
        deployment.factory = _deployFactory(deployment, config);
        deployment.anchor.initializeFactory(address(deployment.factory));
        vm.stopBroadcast();

        _writeDeployment(deployment, config);
    }

    function _readConfig() internal view returns (Config memory config) {
        config.privateKey = vm.envUint("PRIVATE_KEY");
        config.challenger = vm.envOr("WORLD_CHALLENGER_ADDRESS", address(0));
        config.l2ChainId = vm.envUint("WORLD_CHAIN_L2_CHAIN_ID");
        config.rollupConfigHash = vm.envBytes32("ROLLUP_CONFIG_HASH");
        config.blockInterval = vm.envOr("PROOF_SYSTEM_BLOCK_INTERVAL", uint256(10));
        config.proofThreshold = uint8(vm.envOr("PROOF_THRESHOLD", uint256(WorldChainProofLib.PROOF_THRESHOLD)));
    }

    function _deployFactory(Deployment memory deployment, Config memory config)
        internal
        returns (WorldChainProofSystemFactory)
    {
        return new WorldChainProofSystemFactory(
            WorldChainProofLib.Domain({
                chainId: config.l2ChainId,
                proofSystemVersion: 1,
                rollupConfigHash: config.rollupConfigHash,
                blockInterval: config.blockInterval
            }),
            IWorldChainAnchorStateRegistry(address(deployment.anchor)),
            CHALLENGE_PERIOD,
            PROOF_PERIOD,
            PROPOSER_BOND,
            CHALLENGER_BOND,
            config.proofThreshold,
            deployment.validityVerifier,
            deployment.teeVerifier,
            deployment.councilVerifier,
            deployment.staking
        );
    }

    function _writeDeployment(Deployment memory deployment, Config memory config) internal {
        string memory out = vm.envOr("PROOF_SYSTEM_DEPLOYMENT_OUT", string(""));
        if (bytes(out).length == 0) return;

        string memory root = "deployment";
        vm.serializeAddress(root, "anchorStateRegistry", address(deployment.anchor));
        vm.serializeAddress(root, "validityProofVerifier", address(deployment.validityVerifier));
        vm.serializeAddress(root, "teeVerifier", address(deployment.teeVerifier));
        vm.serializeAddress(root, "securityCouncil", address(deployment.councilVerifier));
        vm.serializeAddress(root, "stakingRegistry", address(deployment.staking));
        vm.serializeAddress(root, "proofSystemFactory", address(deployment.factory));
        vm.serializeBytes32(root, "rollupConfigHash", config.rollupConfigHash);
        vm.serializeUint(root, "l2ChainId", config.l2ChainId);
        vm.serializeUint(root, "proofSystemVersion", 1);
        vm.serializeUint(root, "blockInterval", config.blockInterval);
        string memory json = vm.serializeUint(root, "proofThreshold", config.proofThreshold);
        vm.writeJson(json, out);
    }
}
