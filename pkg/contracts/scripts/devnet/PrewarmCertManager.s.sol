// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Script, console} from "forge-std/Script.sol";
import {ICertManager} from "@nitro-validator/ICertManager.sol";

/// @notice Submits the AWS Nitro CA certificate chain to CertManager.
///         Reads a simplified JSON plan (produced by the Justfile's jq pre-processing step)
///         and submits each cert entry that is not already cached on-chain.
///
/// @dev Required env vars:
///   CERT_MANAGER_ADDRESS  — deployed CertManager contract address
///   PREWARM_PLAN          — path to the simplified JSON file (default: "prewarm-plan.json")
///
/// The simplified JSON has the form (parallel arrays, same length):
///   {
///     "calldatas":  ["0x...", "0x...", ...],
///     "certHashes": ["0x...", "0x...", ...]
///   }
/// It contains only cert-caching entries (cache_ca / cache_leaf); validate_attestation
/// entries are filtered out by the Justfile jq step before this script runs.
///
/// Cache check: CertManager.loadVerified(certHash) returns a struct whose pubKey is
/// empty (length 0) when the cert is not yet stored; any non-empty pubKey means cached.
///
/// Gas note: each verifyCACertWithHints call costs ~1.5 M gas.  Run with --slow to
/// send one transaction at a time and wait for confirmation before the next.
contract PrewarmCertManager is Script {
    function run() external {
        address certManager = vm.envAddress("CERT_MANAGER_ADDRESS");
        string memory planPath = vm.envOr("PREWARM_PLAN", string("prewarm-plan.json"));
        string memory json = vm.readFile(planPath);

        // Parallel string arrays – hex-encoded so we can parse with vm.parseBytes / vm.parseBytes32.
        string[] memory calldataHexArr = vm.parseJsonStringArray(json, ".calldatas");
        string[] memory certHashHexArr = vm.parseJsonStringArray(json, ".certHashes");

        uint256 count = calldataHexArr.length;
        require(count == certHashHexArr.length, "plan arrays length mismatch");

        uint256 submitted = 0;
        uint256 skipped = 0;

        // Verify CertManager has code deployed — abort early if not.
        require(certManager.code.length > 0, "CertManager has no code — is the address correct and the contract deployed?");

        vm.startBroadcast();

        for (uint256 i = 0; i < count; i++) {
            bytes32 certHash = vm.parseBytes32(certHashHexArr[i]);

            // Check on-chain cache via low-level staticcall — returns (false, "") for non-existent contracts.
            // An uncached cert returns an ABI-encoded VerifiedCert with empty pubKey, so we must
            // decode the struct and check pubKey.length rather than just checking returnData.length.
            (bool ok, bytes memory returnData) = certManager.staticcall(
                abi.encodeWithSignature("loadVerified(bytes32)", certHash)
            );
            if (ok && returnData.length >= 32) {
                ICertManager.VerifiedCert memory cached = abi.decode(returnData, (ICertManager.VerifiedCert));
                if (cached.pubKey.length != 0) {
                    console.log("  Skipping (already cached): certHash %s", certHashHexArr[i]);
                    skipped++;
                    continue;
                }
            }

            // Submit using the pre-computed ABI-encoded calldata from the plan.
            bytes memory data = vm.parseBytes(calldataHexArr[i]);
            console.log("  Submitting cert %d/%d (certHash %s)", i + 1, count, certHashHexArr[i]);
            (bool success, bytes memory ret) = certManager.call(data);
            if (!success) {
                // Bubble up CertManager's revert reason so the script fails with context.
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
