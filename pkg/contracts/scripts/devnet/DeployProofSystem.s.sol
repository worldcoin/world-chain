// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";

import {WorldChainAnchorStateRegistry} from "../../src/proofs/WorldChainAnchorStateRegistry.sol";
import {WorldChainProofLib} from "../../src/proofs/WorldChainProofLib.sol";
import {WorldChainProofSystemFactory} from "../../src/proofs/WorldChainProofSystemFactory.sol";
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

    uint64 internal constant CHALLENGE_PERIOD = 1 days;
    uint64 internal constant PROOF_PERIOD = 7 days;
    uint256 internal constant PROPOSER_BOND = 1 ether;
    uint256 internal constant CHALLENGER_BOND = 0.1 ether;

    function run() external returns (Deployment memory deployment) {
        uint256 privateKey = vm.envUint("PRIVATE_KEY");
        address challenger = vm.envOr("WORLD_CHALLENGER_ADDRESS", address(0));
        uint256 l2ChainId = vm.envUint("WORLD_CHAIN_L2_CHAIN_ID");
        bytes32 rollupConfigHash = vm.envBytes32("ROLLUP_CONFIG_HASH");
        uint256 blockInterval = vm.envOr("PROOF_SYSTEM_BLOCK_INTERVAL", uint256(10));
        uint256 intermediateBlockInterval = vm.envOr("PROOF_SYSTEM_INTERMEDIATE_BLOCK_INTERVAL", uint256(5));
        uint8 proofThreshold = uint8(vm.envOr("PROOF_THRESHOLD", uint256(WorldChainProofLib.PROOF_THRESHOLD)));

        vm.startBroadcast(privateKey);

        deployment.anchor = new WorldChainAnchorStateRegistry(bytes32(0), 0);
        deployment.staking = new MockStakingRegistry();
        deployment.validityVerifier = new MockRootIdVerifier(false);
        deployment.teeVerifier = new MockRootIdVerifier(false);
        deployment.councilVerifier = new MockRootIdVerifier(false);
        deployment.staking.setStaked(vm.addr(privateKey), true);
        if (challenger != address(0)) {
            deployment.staking.setStaked(challenger, true);
        }

        deployment.factory = new WorldChainProofSystemFactory(
            WorldChainProofLib.Domain({
                chainId: l2ChainId,
                proofSystemVersion: 1,
                rollupConfigHash: rollupConfigHash,
                blockInterval: blockInterval,
                intermediateBlockInterval: intermediateBlockInterval
            }),
            CHALLENGE_PERIOD,
            PROOF_PERIOD,
            PROPOSER_BOND,
            CHALLENGER_BOND,
            proofThreshold,
            deployment.validityVerifier,
            deployment.teeVerifier,
            deployment.councilVerifier,
            deployment.staking
        );

        vm.stopBroadcast();

        _writeDeployment(
            deployment, rollupConfigHash, l2ChainId, blockInterval, intermediateBlockInterval, proofThreshold
        );
    }

    function _writeDeployment(
        Deployment memory deployment,
        bytes32 rollupConfigHash,
        uint256 l2ChainId,
        uint256 blockInterval,
        uint256 intermediateBlockInterval,
        uint8 proofThreshold
    ) internal {
        string memory out = vm.envOr("PROOF_SYSTEM_DEPLOYMENT_OUT", string(""));
        if (bytes(out).length == 0) return;

        string memory root = "deployment";
        vm.serializeAddress(root, "anchorStateRegistry", address(deployment.anchor));
        vm.serializeAddress(root, "validityProofVerifier", address(deployment.validityVerifier));
        vm.serializeAddress(root, "teeVerifier", address(deployment.teeVerifier));
        vm.serializeAddress(root, "securityCouncil", address(deployment.councilVerifier));
        vm.serializeAddress(root, "stakingRegistry", address(deployment.staking));
        vm.serializeAddress(root, "proofSystemFactory", address(deployment.factory));
        vm.serializeBytes32(root, "rollupConfigHash", rollupConfigHash);
        vm.serializeUint(root, "l2ChainId", l2ChainId);
        vm.serializeUint(root, "proofSystemVersion", 1);
        vm.serializeUint(root, "blockInterval", blockInterval);
        vm.serializeUint(root, "proofThreshold", proofThreshold);
        string memory json = vm.serializeUint(root, "intermediateBlockInterval", intermediateBlockInterval);
        vm.writeJson(json, out);
    }
}
