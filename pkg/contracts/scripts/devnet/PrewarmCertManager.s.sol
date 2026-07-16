// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Script, console} from "forge-std/Script.sol";
import {ICertManager} from "@nitro-validator/ICertManager.sol";

/// @notice Submits the AWS Nitro CA certificate chain to CertManager.
///         Reads a JSON plan produced by hinted_attestation_calls.js and submits
///         each cold cert entry that is not already cached on-chain.
///
/// @dev Required env vars:
///   CERT_MANAGER_ADDRESS  — deployed CertManager contract address
///   PREWARM_PLAN          — path to the hinted_attestation_calls.js JSON output
///                           (defaults to "prewarm-plan.json")
///
/// The JSON plan has the following structure (produced by hinted_attestation_calls.js):
///   {
///     "cold": [
///       { "tx": 1, "kind": "cache_ca",   "label": "...", "certHash": "0x...", "calldata": "0x..." },
///       { "tx": 2, "kind": "cache_leaf",  "label": "...", "certHash": "0x...", "calldata": "0x..." },
///       { "tx": 3, "kind": "validate_attestation", "label": "...", "calldata": "0x..." }
///     ]
///   }
/// Only entries with a "certHash" field (i.e. cache_ca and cache_leaf) are processed;
/// the final validate_attestation entry is skipped.
///
/// Cache check: CertManager.loadVerified(certHash) returns a struct with pubKey.length == 0
/// when the cert is not yet cached.  Any non-zero pubKey means it is already stored and
/// the entry is skipped.
///
/// Gas note: each verifyCACertWithHints transaction costs ~1.5 M gas; use --slow to
/// send one transaction at a time and wait for confirmation before the next.
contract PrewarmCertManager is Script {
    function run() external {
        address certManager = vm.envAddress("CERT_MANAGER_ADDRESS");
        string memory planPath = vm.envOr("PREWARM_PLAN", string("prewarm-plan.json"));
        string memory json = vm.readFile(planPath);

        // Determine the number of entries in the cold array.
        // vm.parseJsonKeys returns the keys of a JSON object or the indices of a JSON array.
        string[] memory coldKeys = vm.parseJsonKeys(json, ".cold");
        uint256 count = coldKeys.length;

        uint256 submitted = 0;
        uint256 skipped = 0;

        vm.startBroadcast();

        for (uint256 i = 0; i < count; i++) {
            string memory prefix = string.concat(".cold[", vm.toString(i), "]");

            // Skip entries that are not cert-caching calls (e.g. validate_attestation has no certHash).
            if (!vm.keyExistsJson(json, string.concat(prefix, ".certHash"))) {
                continue;
            }

            bytes32 certHash = vm.parseJsonBytes32(json, string.concat(prefix, ".certHash"));
            string memory label = vm.parseJsonString(json, string.concat(prefix, ".label"));

            // Check on-chain cache: loadVerified returns a zero-length pubKey when not cached.
            ICertManager.VerifiedCert memory cached = ICertManager(certManager).loadVerified(certHash);
            if (cached.pubKey.length != 0) {
                console.log("  Skipping (already cached): %s", label);
                skipped++;
                continue;
            }

            // Submit using the pre-computed ABI-encoded calldata from the plan.
            bytes memory data = vm.parseJsonBytes(json, string.concat(prefix, ".calldata"));
            console.log("  Submitting: %s", label);
            (bool success, bytes memory ret) = certManager.call(data);
            if (!success) {
                // Bubble up the revert reason from CertManager so the script fails with context.
                assembly {
                    revert(add(ret, 32), mload(ret))
                }
            }
            submitted++;
        }

        vm.stopBroadcast();

        console.log("CertManager pre-warm complete:");
        console.log("  Submitted:", submitted);
        console.log("  Skipped (already cached):", skipped);
    }
}
