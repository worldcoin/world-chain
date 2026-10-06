// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Governance} from "./Governance.s.sol";

import {
    SecurityCouncilVerifier
} from "../../src/dispute/council/SecurityCouncilVerifier.sol";

import {Safe} from "@safe-global/safe-contracts/contracts/Safe.sol";
import {
    SafeProxyFactory
} from "@safe-global/safe-contracts/contracts/proxies/SafeProxyFactory.sol";
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
contract DeployCouncilSafe is Governance {
    struct Deployment {
        Safe councilSafe;
        SecurityCouncilVerifier verifier;
        address singleton;
        address proxyFactory;
        address fallbackHandler;
        uint256 threshold;
        uint256 saltNonce;
    }

    function run() external returns (Deployment memory deployment) {
        uint256 privateKey = vm.envUint("PRIVATE_KEY");
        address[] memory owners;
        uint256 threshold;
        if (_safeMode()) {
            (uint256 firstKey, uint256 secondKey) = _signerKeys();
            require(firstKey != 0 && secondKey != 0 && firstKey != secondKey, "Governance: two distinct keys required");
            owners = new address[](2);
            owners[0] = vm.addr(firstKey);
            owners[1] = vm.addr(secondKey);
            threshold = 2;
        } else {
            owners = vm.envAddress("COUNCIL_OWNERS", ",");
            threshold = vm.envOr("COUNCIL_THRESHOLD", uint256(1));
        }
        address existingSafe = _safeMode() ? _safeAddress() : address(0);
        if (_safeMode() && existingSafe != address(0)) {
            deployment.councilSafe = _governanceSafe();
            deployment.threshold = threshold;
            vm.startBroadcast(privateKey);
            deployment.verifier = new SecurityCouncilVerifier(existingSafe);
            vm.stopBroadcast();
            _validateCouncil(deployment);
            _writeDeployment(deployment, owners);
            return deployment;
        }
        // Bump to get a fresh Safe address for the same owner set (CREATE2 salt input).
        uint256 saltNonce = vm.envOr("COUNCIL_SAFE_SALT_NONCE", uint256(0));

        require(
            owners.length > 0,
            "DeployCouncilSafe: COUNCIL_OWNERS is empty"
        );
        require(
            threshold > 0,
            "DeployCouncilSafe: COUNCIL_THRESHOLD must be non-zero"
        );
        require(
            threshold <= owners.length,
            "DeployCouncilSafe: COUNCIL_THRESHOLD exceeds owner count"
        );

        address singleton = vm.envOr("SAFE_SINGLETON", address(0));
        address factory = vm.envOr("SAFE_PROXY_FACTORY", address(0));
        address handler = vm.envOr("SAFE_FALLBACK_HANDLER", address(0));
        bool reuse = singleton != address(0) ||
            factory != address(0) ||
            handler != address(0);
        if (reuse) {
            // Partial reuse silently mixes a fresh singleton with a foreign factory; demand all
            // three or none so the resulting Safe's provenance is unambiguous.
            require(
                singleton != address(0) &&
                    factory != address(0) &&
                    handler != address(0),
                "DeployCouncilSafe: set SAFE_SINGLETON, SAFE_PROXY_FACTORY and SAFE_FALLBACK_HANDLER together"
            );
            require(
                singleton.code.length > 0,
                "DeployCouncilSafe: SAFE_SINGLETON has no code"
            );
            require(
                factory.code.length > 0,
                "DeployCouncilSafe: SAFE_PROXY_FACTORY has no code"
            );
            require(
                handler.code.length > 0,
                "DeployCouncilSafe: SAFE_FALLBACK_HANDLER has no code"
            );
        }

        vm.startBroadcast(privateKey);
        if (!reuse) {
            singleton = address(new Safe());
            factory = address(new SafeProxyFactory());
            handler = address(new CompatibilityFallbackHandler());
        }

        bytes memory initializer = abi.encodeCall(
            Safe.setup,
            (
                owners,
                threshold,
                address(0),
                "",
                handler,
                address(0),
                0,
                payable(address(0))
            )
        );
        deployment.councilSafe = Safe(
            payable(
                address(
                    SafeProxyFactory(factory).createProxyWithNonce(
                        singleton,
                        initializer,
                        saltNonce
                    )
                )
            )
        );

        deployment.verifier = new SecurityCouncilVerifier(
            address(deployment.councilSafe)
        );
        vm.stopBroadcast();

        deployment.singleton = singleton;
        deployment.proxyFactory = factory;
        deployment.fallbackHandler = handler;
        deployment.threshold = threshold;
        deployment.saltNonce = saltNonce;

        require(
            deployment.councilSafe.getThreshold() == threshold,
            "DeployCouncilSafe: threshold not set"
        );
        require(
            address(deployment.verifier.council()) ==
                address(deployment.councilSafe),
            "DeployCouncilSafe: verifier not bound to the council Safe"
        );

        if (_safeMode()) {
            _validateSafe(deployment.councilSafe);
        }
        _validateCouncil(deployment);
        _writeDeployment(deployment, owners);
    }

    function _validateCouncil(Deployment memory deployment) internal view {
        bytes32 rootId = keccak256("devnet council configuration check");
        (bool success, bytes memory result) = address(deployment.councilSafe).staticcall(
            abi.encodeWithSignature("getMessageHash(bytes)", abi.encode(deployment.verifier.attestationDigest(rootId)))
        );
        require(success && result.length == 32, "DeployCouncilSafe: compatibility handler required");
        if (_safeMode()) {
            require(
                deployment.verifier.verify(_safeSignatures(abi.decode(result, (bytes32))), bytes32(0), abi.encode(rootId)),
                "DeployCouncilSafe: council signature check failed"
            );
        }
    }

    function _writeDeployment(
        Deployment memory deployment,
        address[] memory owners
    ) internal {
        string memory out = vm.envOr("COUNCIL_DEPLOYMENT_OUT", string(""));
        if (bytes(out).length == 0) return;

        string memory root = "council";
        vm.serializeAddress(
            root,
            "securityCouncilVerifier",
            address(deployment.verifier)
        );
        vm.serializeAddress(
            root,
            "councilSafe",
            address(deployment.councilSafe)
        );
        vm.serializeAddress(root, "safeSingleton", deployment.singleton);
        vm.serializeAddress(root, "safeProxyFactory", deployment.proxyFactory);
        vm.serializeAddress(
            root,
            "safeFallbackHandler",
            deployment.fallbackHandler
        );
        vm.serializeAddress(root, "councilOwners", owners);
        vm.serializeUint(root, "safeSaltNonce", deployment.saltNonce);
        string memory json = vm.serializeUint(
            root,
            "councilThreshold",
            deployment.threshold
        );
        vm.writeJson(json, out);
    }
}
