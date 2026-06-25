// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Script} from "forge-std/Script.sol";
import {console} from "forge-std/console.sol";
import {CertManager} from "../../src/proofs/nitro/vendor/nitro-validator/CertManager.sol";
import {ICertManager} from "../../src/proofs/nitro/vendor/nitro-validator/ICertManager.sol";
import {NitroAttestationVerifier} from "../../src/proofs/nitro/NitroAttestationVerifier.sol";
import {NitroEnclaveKeyRegistry} from "../../src/proofs/nitro/NitroEnclaveKeyRegistry.sol";
import {NitroProofVerifier} from "../../src/proofs/nitro/NitroProofVerifier.sol";

/// @title DeployNitro
/// @notice Deploys the on-chain AWS Nitro attestation stack for WIP-1006:
///         {CertManager} → {NitroAttestationVerifier} → {NitroEnclaveKeyRegistry} →
///         {NitroProofVerifier}.
///
/// @dev Operators MUST pre-warm {CertManager} with each intermediate CA cert in the
///      AWS Nitro PKI by calling `verifyCACert(cert, parentCertHash)` separately,
///      starting from the root, before any user calls
///      {NitroEnclaveKeyRegistry.registerKey}. Pre-warming amortizes the ~63M-gas
///      cost of an X.509+P-384 chain validation across many attestation
///      registrations.
contract DeployNitro is Script {
    function run() external {
        address owner = vm.envAddress("OWNER");
        vm.startBroadcast();

        CertManager certManager = new CertManager();
        console.log("CertManager:", address(certManager));

        NitroAttestationVerifier verifier =
            new NitroAttestationVerifier(ICertManager(address(certManager)));
        console.log("NitroAttestationVerifier:", address(verifier));

        NitroEnclaveKeyRegistry registry = new NitroEnclaveKeyRegistry(verifier, owner);
        console.log("NitroEnclaveKeyRegistry:", address(registry));

        NitroProofVerifier proofVerifier = new NitroProofVerifier(registry);
        console.log("NitroProofVerifier:", address(proofVerifier));

        vm.stopBroadcast();
    }
}
