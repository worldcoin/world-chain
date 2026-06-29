// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Script} from "forge-std/Script.sol";
import {console} from "forge-std/console.sol";
import {CertManager} from "@nitro-validator/CertManager.sol";
import {ICertManager} from "@nitro-validator/ICertManager.sol";
import {NitroAttestationVerifier} from "../../src/proofs/nitro/NitroAttestationVerifier.sol";
import {NitroEnclaveKeyRegistry} from "../../src/proofs/nitro/NitroEnclaveKeyRegistry.sol";
import {NitroProofVerifier} from "../../src/proofs/nitro/NitroProofVerifier.sol";

/// @title DeployNitro
/// @notice Deploys the on-chain AWS Nitro attestation stack for WIP-1006:
///         {CertManager} → {NitroAttestationVerifier} → {NitroEnclaveKeyRegistry}
///         → {NitroProofVerifier}.
///
/// @dev Required environment variables:
///        - `OWNER` — address that becomes the owner of both
///          {NitroAttestationVerifier} (manages the PCR allowlist) and
///          {NitroEnclaveKeyRegistry} (revokes keys).
///
///      ## REQUIRED follow-up: approve the initial PCR set(s)
///      {NitroAttestationVerifier} is deployed with an empty allowlist, so
///      `verifyAttestation` will revert with `PCRSetNotApproved` until the
///      owner approves at least one enclave image. Immediately after this
///      script runs, the owner MUST submit:
///
///        verifier.approvePCRSet(keccak256(rawPcr0), keccak256(rawPcr1),
///                               keccak256(rawPcr2))
///
///      for every approved EIF. The raw PCRs are the 48-byte SHA-384 hashes
///      reported by `nitro-cli describe-eif`.
///
///      ## Operator pre-warm step (required before first registerKey)
///      {CertManager} caches verified certificates so the ~63M-gas X.509 +
///      P-384 chain check is paid only once per intermediate. Operators MUST
///      pre-warm by calling `verifyCACert(cert, parentCertHash)` for every
///      intermediate in the AWS Nitro PKI (root → top-level intermediate →
///      … → leaf's immediate issuer) in separate transactions before any
///      user calls {NitroEnclaveKeyRegistry.registerKey}.
///
///      ## Enclave image upgrade flow
///      When a new enclave image (EIF) is built:
///        1. Capture its PCR0/1/2 measurements; compute their keccak256.
///        2. `verifier.approvePCRSet(newPcr0, newPcr1, newPcr2)` (owner-only).
///        3. Roll out new enclaves; each registers via
///           {NitroEnclaveKeyRegistry.registerKey}, which calls
///           {NitroAttestationVerifier.verifyAttestation}. The verifier
///           accepts both old- and new-image attestations during overlap.
///        4. After migration, `verifier.revokePCRSet(oldPcr0, oldPcr1,
///           oldPcr2)` to stop accepting new registrations for the retired
///           image. Already-registered keys remain in the registry until
///           individually revoked via `registry.revokeKey(pubkey)`.
contract DeployNitro is Script {
    function run() external {
        address owner = vm.envAddress("OWNER");

        vm.startBroadcast();

        CertManager certManager = new CertManager();
        console.log("CertManager:", address(certManager));

        NitroAttestationVerifier verifier =
            new NitroAttestationVerifier(ICertManager(address(certManager)), owner);
        console.log("NitroAttestationVerifier:", address(verifier));

        NitroEnclaveKeyRegistry registry = new NitroEnclaveKeyRegistry(verifier, owner);
        console.log("NitroEnclaveKeyRegistry:", address(registry));

        NitroProofVerifier proofVerifier = new NitroProofVerifier(registry);
        console.log("NitroProofVerifier:", address(proofVerifier));

        vm.stopBroadcast();

        console.log("");
        console.log("NEXT STEPS (owner):");
        console.log("  1. Pre-warm CertManager with the AWS Nitro PKI intermediates.");
        console.log("  2. verifier.approvePCRSet(pcr0, pcr1, pcr2) for each approved EIF.");
    }
}
