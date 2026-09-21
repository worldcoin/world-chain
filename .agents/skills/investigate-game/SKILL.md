---
name: investigate-game
description: Verify an Ethereum address is a WIP1006 dispute game and report its status, claim, bonds, proof configuration, and proposal context in a compact debugging table.
---

Investigate one game address using read-only Ethereum RPC calls. Do not submit
transactions or trace its ancestors unless separately requested.

## Network and snapshot

Require a `0x`-prefixed, 40-hex-digit address. Default to Ethereum mainnet and
`ETHEREUM_PROVIDER`; use `ETHEREUM_SEPOLIA_PROVIDER` when Sepolia is specified.
Honor an explicitly supplied network/provider instead. If the provider is missing,
ask for it without requesting credentials in chat. Never print provider URLs or
raw provider errors that may contain credentials.

Use Foundry `cast` with `--rpc-timeout 20` for remote calls. Verify the RPC chain ID
(mainnet: 1; Sepolia: 11155111), capture one latest L1 block number, hash, and
timestamp, and pin every contract/code read to that block with `--block`.
Recheck its hash before reporting; if it changed, restart once with a new snapshot,
then report an unstable snapshot if it changes again. Do not mix reads from
different blocks. No automatic retries are needed for failed RPC calls.

## Verify before investigating

1. Read bytecode with `cast code` and call `gameType()(uint32)` at the address.
   WIP1006 is game type **1006**, defined in
   `pkg/contracts/src/dispute/lib/GameTypes.sol` (paths are repository-relative).
2. If there is no bytecode, the getter definitively reverts/returns invalid ABI,
   or the decoded type is not 1006, stop immediately and return only:
   **“This is not a WIP1006 game contract address.”**
   A timeout, rate limit, unavailable historical state, wrong network, or other
   RPC failure is inconclusive: report that verification failed, not that the
   address is a different contract type.
3. Check that this is a registered game instance: read
   `disputeGameFactory()(address)`, `rootClaim()(bytes32)`, and `extraData()(bytes)`;
   on that factory call `games(uint32,bytes32,bytes)(address,uint64)` with type
   1006 and the exact root/extra data. Require its returned address to equal the
   input address and its creation timestamp to equal `createdAt()(uint64)` and
   be nonzero. A definitive registration mismatch receives the same immediate
   rejection above. Do not compare against only the factory's current
   implementation: valid older games can use a previous implementation.
4. When a trusted factory for the requested deployment is available from user
   context or repository deployment configuration, require the reported factory
   to match it. Do not apply a deployment file to a different network. Without
   that independent reference, disclose that verification establishes type and
   factory registration only; a contract's self-reported factory does not prove
   canonical World Chain provenance. Inconclusive verification must not be
   reported as authenticated identity.

## Read and decode

Use the following ABI signatures. For example, after assigning the validated
address, selected provider, and snapshot block to shell variables:

```sh
cast call "$game_address" 'claimData()(uint8,address,uint64,uint8,uint8)' \
  --rpc-url "$game_rpc" --rpc-timeout 20 --block "$game_block"
```

Collect independent reads in parallel with bounded concurrency (at most four),
or in a JSON-RPC batch if supported. Inspect every result. Never substitute zero
for a failed read. After identity verification, preserve successful fields if an
individual getter fails, mark the report incomplete, and identify the failed
getter without exposing provider credentials.

| Output field | ABI signature | Presentation |
| --- | --- | --- |
| GameStatus | `status()(uint8)` | Decoded name and numeric value |
| challengerBond | `challengerBond()(uint256)` | Required challenge bond, exact raw token units |
| proposerBond | `proposerBond()(uint256)` | Required proposal bond, exact raw token units |
| aggregationVKey | `aggregationVKey()(bytes32)` | Full SP1 aggregation verification key |
| rangeVKeyCommitment | `rangeVKeyCommitment()(bytes32)` | Full SP1 range verification-key commitment |
| teeImageId | `teeImageId()(bytes32)` | Full Nitro PCR0 image identity commitment |
| validityProofVerifier | `validityProofVerifier()(address)` | SP1 lane verifier address |
| teeVerifier | `teeVerifier()(address)` | Nitro lane verifier address |
| securityCouncil | `securityCouncil()(address)` | Council verifier address; do not assume it is a Safe |
| createdAt | `createdAt()(uint64)` | Unix seconds and UTC date/time |
| claimData.status | `claimData()(uint8,address,uint64,uint8,uint8)` | Tuple item 1: proposal status |
| claimData.challenger | Same tuple | Item 2: full address; label zero as no challenger |
| claimData.deadline | Same tuple | Item 3: Unix seconds, UTC, and time remaining/elapsed at snapshot |
| claimData.proofBitmap | Same tuple | Item 4: hex, decimal, and accepted lane names |
| claimData.invalidationReason | Same tuple | Item 5: decoded name and numeric value |
| rootClaim | `rootClaim()(bytes32)` | Full claimed output root |
| l2SequenceNumber | `l2SequenceNumber()(uint256)` | Claimed L2 block number |
| gameCreator | `gameCreator()(address)` | Proposer address |
| l1Head | `l1Head()(bytes32)` | Parent hash of the L1 block at creation |
| parentRef | `parentRef()(address)` | Parent reference; can be the anchor registry |
| attempt | `attempt()(uint256)` | Retry attempt |
| proposalDomainHash | `proposalDomainHash()(bytes32)` | Full committed domain hash |

Fetch `claimData()` once and unpack it into the five rows.

Decode using these mappings from `IMultiProofGame.sol` and `lib/LibProof.sol` under
`pkg/contracts/src/dispute/`:

- GameStatus: 0 = `IN_PROGRESS`, 1 = `CHALLENGER_WINS`, 2 = `DEFENDER_WINS`.
- Proposal status: 0 = `Unchallenged`, 1 = `Challenged`,
  2 = `UnchallengedAndValidProofProvided`,
  3 = `ChallengedAndValidProofProvided`, 4 = `Resolved`.
- Invalidation reason: 0 = `NONE`, 1 = `PROOF_TIMEOUT`, 2 = `INVALID_PARENT`.
- Proof bitmap: `0x01` = `VALIDITY_PROOF`, `0x02` = `TEE_ATTESTATION`,
  `0x04` = `SECURITY_COUNCIL`; zero means no accepted lanes. These are bit masks,
  not lane IDs. Preserve and flag unknown bits or enum values instead of guessing.

Bonds in this implementation are ERC-20 amounts, not ETH. Read
`bondVault()(address)` and its `token()(address)` to identify the asset. If token
`decimals()(uint8)` is available, also show exact decimal amounts using integer or
decimal arithmetic, never floating point. Symbol metadata is optional/untrusted.
If asset metadata is unavailable, retain raw units and state that limitation.

Read `anchorStateRegistry()(address)` to label `parentRef` as the anchor registry
when equal; otherwise label it as the parent game reference. Do not recurse.
The council address is a verifier and may wrap a Safe; do not label it a verified
Safe without separate evidence. The current claim deadline is not necessarily
the original challenge deadline. Expiry alone does not change the stored
GameStatus; report the actual status without predicting a resolved outcome.

## Response

Start with the full game address, network/chain ID, and snapshot L1 block number,
hash, and UTC timestamp. State the verification result and factory address,
including any provenance limitation. Then present one compact two-column
**Field / Value** table with all rows above, grouped in that order. Identify the
bond token in the bond rows or one short note. Keep hashes and addresses complete
and copyable; do not truncate them. Use short notes only for anomalies, incomplete
reads, or decoding limitations. Do not dump raw RPC JSON or expand into a general
protocol explanation.
