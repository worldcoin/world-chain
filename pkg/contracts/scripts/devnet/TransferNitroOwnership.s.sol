// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Governance} from "./Governance.s.sol";
import {CertManager} from "@nitro-validator/CertManager.sol";
import {NitroAttestationVerifier} from "../../src/dispute/nitro/NitroAttestationVerifier.sol";
import {NitroEnclaveKeyRegistry} from "../../src/dispute/nitro/NitroEnclaveKeyRegistry.sol";

/// @notice Explicit handoff of a reused Nitro stack; GOVERNANCE_MODE describes its current owner.
contract TransferNitroOwnership is Governance {
    struct Config {
        address newOwner;
        uint256 ownerKey;
        CertManager certManager;
        NitroAttestationVerifier verifier;
        NitroEnclaveKeyRegistry registry;
    }

    function _readConfig() internal view virtual returns (Config memory) {
        return Config(
            vm.envAddress("NEW_NITRO_OWNER"),
            vm.envOr("OWNER_KEY", uint256(0)),
            CertManager(vm.envAddress("CERT_MANAGER_ADDRESS")),
            NitroAttestationVerifier(vm.envAddress("NITRO_ATTESTATION_VERIFIER")),
            NitroEnclaveKeyRegistry(vm.envAddress("NITRO_ENCLAVE_KEY_REGISTRY"))
        );
    }

    function run() external {
        Config memory config = _readConfig();
        address newOwner = config.newOwner;
        require(newOwner != address(0), "Nitro handoff: new owner required");
        uint256 ownerKey = config.ownerKey;
        address currentOwner = _governanceAddress(ownerKey);
        CertManager certManager = config.certManager;
        NitroAttestationVerifier verifier = config.verifier;
        NitroEnclaveKeyRegistry registry = config.registry;
        require(
            certManager.owner() == currentOwner || certManager.owner() == newOwner,
            "Nitro handoff: CertManager owner mismatch"
        );
        require(
            verifier.owner() == currentOwner || verifier.owner() == newOwner, "Nitro handoff: verifier owner mismatch"
        );
        require(
            registry.owner() == currentOwner || registry.owner() == newOwner, "Nitro handoff: registry owner mismatch"
        );
        require(address(registry.verifier()) == address(verifier), "Nitro handoff: verifier wiring mismatch");
        require(address(verifier.certManager()) == address(certManager), "Nitro handoff: CertManager wiring mismatch");
        // Revoker is independent; require the current governance authority before moving it.
        require(
            certManager.revoker() == currentOwner || certManager.revoker() == newOwner,
            "Nitro handoff: revoker mismatch"
        );
        if (certManager.owner() != newOwner) {
            _executeGovernance(ownerKey, address(certManager), abi.encodeCall(CertManager.setRevoker, (newOwner)));
            _executeGovernance(
                ownerKey, address(certManager), abi.encodeCall(CertManager.transferOwnership, (newOwner))
            );
        }
        if (verifier.owner() != newOwner) {
            _executeGovernance(
                ownerKey, address(verifier), abi.encodeWithSignature("transferOwnership(address)", newOwner)
            );
        }
        if (registry.owner() != newOwner) {
            _executeGovernance(
                ownerKey, address(registry), abi.encodeWithSignature("transferOwnership(address)", newOwner)
            );
        }
        if (_preparingSafe()) return;
        require(
            certManager.owner() == newOwner && certManager.revoker() == newOwner,
            "Nitro handoff: CertManager handoff failed"
        );
        require(verifier.owner() == newOwner && registry.owner() == newOwner, "Nitro handoff: handoff failed");
    }
}
