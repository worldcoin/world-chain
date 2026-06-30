// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Script} from "forge-std/Script.sol";
import {console} from "forge-std/console.sol";
import {CertManager} from "@nitro-validator/CertManager.sol";
import {ICertManager} from "@nitro-validator/ICertManager.sol";
import {IP384Verifier} from "@nitro-validator/IP384Verifier.sol";
import {P384Verifier} from "@nitro-validator/P384Verifier.sol";
import {NitroAttestationVerifier} from "../../src/proofs/nitro/NitroAttestationVerifier.sol";
import {NitroEnclaveKeyRegistry} from "../../src/proofs/nitro/NitroEnclaveKeyRegistry.sol";
import {NitroProofVerifier} from "../../src/proofs/nitro/NitroProofVerifier.sol";

/// @title DeployNitro
/// @notice Deploys the on-chain AWS Nitro attestation stack for WIP-1006:
///         `P384Verifier` → `CertManager` → `NitroAttestationVerifier`
///         → `NitroEnclaveKeyRegistry` → `NitroProofVerifier`.
///
/// @dev Required environment variables:
///        - `OWNER` — address that becomes the owner of both
///          `NitroAttestationVerifier` (manages the PCR allowlist) and
///          `NitroEnclaveKeyRegistry` (revokes keys).
///
///      ## Hinted P-384 verification (gas optimisation)
///      Signature verification uses off-chain-computed modular-inverse hints
///      (base/nitro-validator PR #28) to cut the P-384 cost from ~8 M gas to
///      ~1.5 M gas (~75 % saving). Hints are supplied by the caller at
///      registration time and re-verified on-chain, so a wrong hint can only
///      cause a revert, never a false accept.
///      Pre-compute hints with `lib/nitro-validator/tools/p384_hints.js`.
///
///      ## REQUIRED follow-up: approve the initial PCR set(s)
///      `NitroAttestationVerifier` is deployed with an empty allowlist, so
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
///      `CertManager` caches verified certificates so the expensive X.509 +
///      P-384 chain check is paid only once per intermediate. Operators MUST
///      pre-warm by calling `verifyCACertWithHints(cert, parentCertHash, hints)`
///      for every intermediate in the AWS Nitro PKI (root → top-level
///      intermediate → … → leaf's immediate issuer) in separate transactions
///      before any user calls `NitroEnclaveKeyRegistry.registerKey`.
///      Use `lib/nitro-validator/tools/hinted_attestation_calls.js` to
///      generate the full call sequence with pre-computed hints.
///
///      ## Enclave image upgrade flow
///      When a new enclave image (EIF) is built:
///        1. Capture its PCR0/1/2 measurements; compute their keccak256.
///        2. `verifier.approvePCRSet(newPcr0, newPcr1, newPcr2)` (owner-only).
///        3. Roll out new enclaves; each registers via
///           `NitroEnclaveKeyRegistry.registerKey`, which calls
///           `NitroAttestationVerifier.verifyAttestation`. The verifier
///           accepts both old- and new-image attestations during overlap.
///        4. After migration, `verifier.revokePCRSet(oldPcr0, oldPcr1,
///           oldPcr2)` to stop accepting new registrations for the retired
///           image. Already-registered keys remain in the registry until
///           individually revoked via `registry.revokeKey(pubkey)`.
contract DeployNitro is Script {
    function run() external {
        address owner = vm.envAddress("OWNER");

        vm.startBroadcast();

        // Deploy the stateless P-384 hint verifier (shared by CertManager and NitroValidator).
        P384Verifier p384Verifier = new P384Verifier();
        console.log("P384Verifier:", address(p384Verifier));

        CertManager certManager = new CertManager(IP384Verifier(address(p384Verifier)));
        console.log("CertManager:", address(certManager));

        NitroAttestationVerifier verifier =
            new NitroAttestationVerifier(ICertManager(address(certManager)), IP384Verifier(address(p384Verifier)), owner);
        console.log("NitroAttestationVerifier:", address(verifier));

        NitroEnclaveKeyRegistry registry = new NitroEnclaveKeyRegistry(verifier, owner);
        console.log("NitroEnclaveKeyRegistry:", address(registry));

        NitroProofVerifier proofVerifier = new NitroProofVerifier(registry);
        console.log("NitroProofVerifier:", address(proofVerifier));

        vm.stopBroadcast();

        // ════════════════════════════════════════════════════════════════════
        // IMPORTANT: NEXT STEP — pre-warm CertManager *before* any user call
        // ════════════════════════════════════════════════════════════════════
        // CertManager caches verified X.509 certs so the expensive chain check
        // is paid once per intermediate, not once per attestation. Skipping
        // the pre-warm means the FIRST `NitroEnclaveKeyRegistry.registerKey`
        // call will attempt full chain validation inside a single tx and OOG.
        //
        // Use `lib/nitro-validator/tools/hinted_attestation_calls.js` to
        // generate the full pre-warm + registration call sequence with hints.
        //
        // Owner script outline (run before opening up registerKey):
        //
        //   bytes memory rootHints = <p384_hints.js cert --cert root.der --pubkey root_pubkey.hex>;
        //   bytes32 rootHash = certManager.verifyCACertWithHints(rootCertDer, 0, rootHints);
        //   bytes memory imHints = <p384_hints.js cert --cert im.der --pubkey rootPubKey.hex>;
        //   bytes32 imHash   = certManager.verifyCACertWithHints(intermediateDer, rootHash, imHints);
        //   bytes memory issHints = <p384_hints.js cert --cert iss.der --pubkey imPubKey.hex>;
        //   bytes32 issHash  = certManager.verifyCACertWithHints(issuerDer, imHash, issHints);
        //   // …repeat for every additional intermediate, parent-hash chained.
        //
        // Then approve at least one PCR set so verifyAttestation can succeed:
        //
        //   verifier.approvePCRSet(
        //       keccak256(rawPcr0), keccak256(rawPcr1), keccak256(rawPcr2)
        //   );
        //
        // (rawPcr* are the 48-byte SHA-384 values from `nitro-cli describe-eif`.)
        // ════════════════════════════════════════════════════════════════════
        console.log("");
        console.log("NEXT STEPS (owner) — mandatory before any registerKey call:");
        console.log("  1. certManager.verifyCACertWithHints(cert, parentHash, hints) for each cert");
        console.log("     in the AWS Nitro PKI chain (root -> intermediates -> issuer).");
        console.log("     Use tools/hinted_attestation_calls.js to generate calls + hints.");
        console.log("     Without this, the first registerKey will OOG.");
        console.log("  2. verifier.approvePCRSet(pcr0, pcr1, pcr2) for each approved EIF.");
    }
}
