---
name: check-wip1006-config
description: Inspect the active WIP-1006 factory configuration from an L1 AnchorStateRegistry at one finalized snapshot, including safety state, Nitro PCR approvals, and optional implementation comparison.
---

Run `python3 scripts/check_config.py <anchor_state_registry> <mainnet|sepolia> [l1_rpc] [--implementation <address>]`, resolving the script relative to this skill directory. Execute directly; do not read/recreate the script unless troubleshooting. Requires Python standard library, Foundry `cast` (local Keccak only), and the adjacent investigate-game RPC helper. GitHub CLI authentication (`gh auth login`) or `GH_TOKEN`/`GITHUB_TOKEN` with repository push access lets the release lookup include draft releases. No transactions are sent.

RPC defaults: `ETHEREUM_PROVIDER` for mainnet, `ETHEREUM_SEPOLIA_PROVIDER` for sepolia. An explicit URL overrides the environment. Never expose URLs/credentials. Wrong chain IDs fail. Reads use an EIP-1898 canonical block hash, preferably finalized; unsupported finalized tags fall back explicitly to latest. Historical state, JSON-RPC batches and event queries are required. No unpinned state fallback is permitted.

Return the script's compact text report directly, preserving complete addresses/hashes, UNKNOWN fields, checks, provenance, and snapshot metadata. Exit 1 means incomplete or inconsistent configuration; partial output is useful but is not a clean bill of health. Do not infer missing values from existing games or deployment files. If factory discovery fails, report the failed registry getter rather than guessing a deployment.

The script discovers registry → factory → `gameImpls(1006)`, SystemConfig/SuperchainConfig, bond vault/token, SP1 backend, council authority, Nitro key registry → attestation verifier → certificate/P384 dependencies. It checks registration, immutable relationships, constructor constraints, proof domain, ETH init bond (must be zero with this ERC-20 architecture), pauses and PCR approvals. Game type 1006 and domain encoding version 1 are protocol identifiers from repository source, not deployment configuration.

`--implementation` adds an OLD (currently registered) vs NEW (supplied candidate) comparison of all collected implementation/dependency parameters at the same snapshot, including unchanged values. Changes are not classified as safe/unsafe. The candidate is not assumed activated.

Active means the implementation registered at the snapshot, used by games created after its activation. `setImplementation()` does not update existing games. Existing game parameters are never evidence of current factory configuration.

PCR0/1/2 are **Keccak digests of raw PCRs**, not raw SHA-384 PCR bytes. The implementation pins only PCR0 (`teeImageId`); there is no unique expected PCR1/2. Approval events supply candidate triples, and `isPCRSetApproved` verifies current state. Scanning is bounded (64 requests and 180 seconds per verifier); unavailable/incomplete history means UNKNOWN, never “not approved.” A missing approved triple blocks new registrations, but PCR revocation does not revoke already registered signers.

The report identifies the `proofs/...` release matching the active implementation's aggregation vkey, range vkey commitment, and `teeImageId`. The script reads release notes from the [world-chain releases](https://github.com/worldcoin/world-chain/releases) via GitHub's API and hashes each release's raw PCR0 before comparing it with the onchain image ID. All three values must match one release. Draft releases require GitHub authentication with push access; without confirmed draft visibility, a search with no published match reports UNKNOWN because drafts could match. A complete search with no match reports that the onchain proofs release doesn't match any release in the world-chain releases section. Missing onchain values, unavailable GitHub data, malformed release notes, or an incomplete search also report UNKNOWN. This is release metadata identification, not proof artifact verification.

Limitations: this inspects the repository's contract interfaces, not arbitrary future implementations. Getter compatibility/code presence cannot authenticate deployed bytecode, prove key/artifact correctness, test a proof, or certify end-to-end availability. Custom verifier adapters may yield UNKNOWN dependencies. The supplied registry is the trust root, not independently authenticated as a canonical deployment. No automatic network switch. No signer inventory, private keys, certificate cache enumeration, or existing-game inspection. RPC completeness is trusted for historical logs.

Examples:

```text
$check-wip1006-config 0x... sepolia
$check-wip1006-config 0x... mainnet https://... --implementation 0x...
```

Example report shape (illustrative, not live data; actual addresses/hashes are untruncated):

```text
WIP-1006 Active Configuration
Network: sepolia | Chain ID: 11155111
Snapshot: <number> | <hash> | <UTC timestamp> | finalized
...
WIP-1006
implementation: <address>
proposerBond: <integer> [raw ERC-20 units]
...
TEE Attestation
PCR0=<digest> PCR1=<digest> PCR2=<digest> approved=True
...
Onchain proofs release: proofs/v1.0.0-rc.4
Release source: https://api.github.com/repos/worldcoin/world-chain/releases
...
Checks
[OK] Factory init bond is zero (ETH); proposer bond uses the ERC-20 vault
[UNKNOWN] Proof artifacts/key correctness cannot be established from getters
...
Discovered via
implementation: factory.gameImpls(1006)
...
```
