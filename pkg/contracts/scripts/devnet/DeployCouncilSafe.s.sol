// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";

import {SecurityCouncilVerifier} from "../../src/proofs/council/SecurityCouncilVerifier.sol";

import {Safe} from "@safe-global/safe-contracts/contracts/Safe.sol";
import {SafeProxyFactory} from "@safe-global/safe-contracts/contracts/proxies/SafeProxyFactory.sol";
import {
    CompatibilityFallbackHandler
} from "@safe-global/safe-contracts/contracts/handler/CompatibilityFallbackHandler.sol";

/// @notice Deploys the security-council Safe and the `SecurityCouncilVerifier` that fronts it, for
///         use as `SECURITY_COUNCIL_VERIFIER` in `DeployProofSystem.s.sol`.
///
/// Required:
///   `PRIVATE_KEY`        — deployer
///   `COUNCIL_OWNERS`     — comma-separated owner addresses
///   `COUNCIL_THRESHOLD`  — signatures required
///
/// Optional — reuse canonical Safe 1.4.1 infrastructure instead of deploying fresh copies.
/// Set all three together on a chain where Safe is already deployed:
///   `SAFE_SINGLETON`, `SAFE_PROXY_FACTORY`, `SAFE_FALLBACK_HANDLER`
///
/// @dev The fallback handler is mandatory, not cosmetic: `SecurityCouncilVerifier` reaches the Safe
///      through EIP-1271, which only exists on `CompatibilityFallbackHandler`. A Safe set up with
///      a zero handler makes every council `verify` return false and silently disables the lane,
///      so this script refuses to configure one.
contract DeployCouncilSafe is Script {
    struct Deployment {
        Safe councilSafe;
        SecurityCouncilVerifier verifier;
        address singleton;
        address proxyFactory;
        address fallbackHandler;
        uint256 threshold;
    }

    function run() external returns (Deployment memory deployment) {
        uint256 privateKey = vm.envUint("PRIVATE_KEY");
        address[] memory owners = vm.envAddress("COUNCIL_OWNERS", ",");
        uint256 threshold = vm.envUint("COUNCIL_THRESHOLD");

        require(owners.length > 0, "DeployCouncilSafe: COUNCIL_OWNERS is empty");
        require(threshold > 0, "DeployCouncilSafe: COUNCIL_THRESHOLD must be non-zero");
        require(threshold <= owners.length, "DeployCouncilSafe: COUNCIL_THRESHOLD exceeds owner count");

        address singleton = vm.envOr("SAFE_SINGLETON", address(0));
        address factory = vm.envOr("SAFE_PROXY_FACTORY", address(0));
        address handler = vm.envOr("SAFE_FALLBACK_HANDLER", address(0));
        bool reuse = singleton != address(0) || factory != address(0) || handler != address(0);
        if (reuse) {
            // Partial reuse silently mixes a fresh singleton with a foreign factory; demand all
            // three or none so the resulting Safe's provenance is unambiguous.
            require(
                singleton != address(0) && factory != address(0) && handler != address(0),
                "DeployCouncilSafe: set SAFE_SINGLETON, SAFE_PROXY_FACTORY and SAFE_FALLBACK_HANDLER together"
            );
            require(singleton.code.length > 0, "DeployCouncilSafe: SAFE_SINGLETON has no code");
            require(factory.code.length > 0, "DeployCouncilSafe: SAFE_PROXY_FACTORY has no code");
            require(handler.code.length > 0, "DeployCouncilSafe: SAFE_FALLBACK_HANDLER has no code");
        }

        vm.startBroadcast(privateKey);
        if (!reuse) {
            singleton = address(new Safe());
            factory = address(new SafeProxyFactory());
            handler = address(new CompatibilityFallbackHandler());
        }

        deployment.councilSafe =
            Safe(payable(address(SafeProxyFactory(factory).createProxyWithNonce(singleton, "", 0))));
        deployment.councilSafe.setup(owners, threshold, address(0), "", handler, address(0), 0, payable(address(0)));

        deployment.verifier = new SecurityCouncilVerifier(address(deployment.councilSafe));
        vm.stopBroadcast();

        deployment.singleton = singleton;
        deployment.proxyFactory = factory;
        deployment.fallbackHandler = handler;
        deployment.threshold = threshold;

        require(deployment.councilSafe.getThreshold() == threshold, "DeployCouncilSafe: threshold not set");
        require(
            address(deployment.verifier.council()) == address(deployment.councilSafe),
            "DeployCouncilSafe: verifier not bound to the council Safe"
        );

        _writeDeployment(deployment, owners);
    }

    function _writeDeployment(Deployment memory deployment, address[] memory owners) internal {
        string memory out = vm.envOr("COUNCIL_DEPLOYMENT_OUT", string(""));
        if (bytes(out).length == 0) return;

        string memory root = "council";
        vm.serializeAddress(root, "securityCouncilVerifier", address(deployment.verifier));
        vm.serializeAddress(root, "councilSafe", address(deployment.councilSafe));
        vm.serializeAddress(root, "safeSingleton", deployment.singleton);
        vm.serializeAddress(root, "safeProxyFactory", deployment.proxyFactory);
        vm.serializeAddress(root, "safeFallbackHandler", deployment.fallbackHandler);
        vm.serializeAddress(root, "councilOwners", owners);
        string memory json = vm.serializeUint(root, "councilThreshold", deployment.threshold);
        vm.writeJson(json, out);
    }
}
