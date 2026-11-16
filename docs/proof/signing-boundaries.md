# Proof signing boundaries

[BLO-1336](https://linear.app/worldcoin/issue/BLO-1336) tracks this audit and the
remaining dependency remediation. Source baseline: `868854ed622bde422afa319a81d2e002e84f71a8`.
This is a source audit, not evidence of deployed keys, enabled backends or runtime qualification.

## Deployment scope

Operator clarification on 2026-11-15: **SP1 is used on devnet only, not on
testnet or mainnet**. Its SDK, Network account, deposit wallet and guest-program
migration are a separate devnet workstream, not a prerequisite for testnet/mainnet
signing remediation.

Assess testnet/mainnet against the actual deployed binaries, enabled signing roles
and signing backends. Neither a repository lockfile nor compilation of an unused
SDK proves its secret-signing path is active. Establish Nitro and other proof-service
deployment separately; their inclusion below is a source inventory, not a claim
that they run on testnet/mainnet.

## Roles

| Role | Current implementation and authority | Integration decision |
| --- | --- | --- |
| Nitro proof-result key | [`enclave.rs`](../../proofs/measured/nitro-enclave/src/enclave.rs) generates an ephemeral secp256k1 key from NSM entropy at startup. `handle_range` derives transition public values by executing the range, checks any supplied expectation, then signs their ABI commitment. The public key is bound to PCRs by NSM attestation. | Keep the key and result signing inside this measured proof enclave. A general external digest signer would lose the binding between the registered key and the computation. |
| SP1 range/aggregation proof (devnet only) | [Range guest](../../proofs/measured/sp1-programs/range-ethereum/src/main.rs) executes the range and commits its output; [aggregation guest](../../proofs/measured/sp1-programs/aggregation/src/main.rs) verifies range proofs and commits aggregation public values. | These guests do not use a service wallet to sign their proof results. Preserve their guest programs and verification-key binding when the devnet stack changes. |
| Proposer L1 wallet | [`proposer/src/main.rs`](../../proofs/services/proposer/src/main.rs): `PROPOSER_KEY` or `PROPOSER_KMS_KEY_ID`; signs game creation and lifecycle transactions. | Existing native KMS is an available backend. A future restricted signer must cover the actual lifecycle calls, not just proposal creation. |
| Challenger L1 wallet | [`challenger/src/main.rs`](../../proofs/services/challenger/src/main.rs): `CHALLENGER_KEY` or `CHALLENGER_KMS_KEY_ID`; signs challenges and lifecycle transactions. | Separate wallet and permissions from proposer/defender; native KMS already supported. |
| Defender L1 wallet | [`defender/src/main.rs`](../../proofs/services/defender/src/main.rs): `DEFENDER_KEY` or `DEFENDER_KMS_KEY_ID`; submits completed proof lanes and lifecycle transactions. | Paying for/submitting a proof does not make this wallet its proof-result key. Native KMS already supported. |
| Nitro registration L1 wallet | [`register/src/lib.rs`](../../proofs/backends/nitro/register/src/lib.rs) and [worker selection](../../proofs/workers/nitro/src/cmd/mod.rs): dedicated `REGISTER_PRIVATE_KEY` or `REGISTER_KMS_KEY_ID`, otherwise legacy `PRIVATE_KEY`. Pays for key registration. | Does not possess the enclave key. Preserve rejection of conflicting dedicated inputs and the existing explicit precedence rules. |
| SP1 Network account (devnet only) | [`network_prover.rs`](../../proofs/backends/sp1/host/src/network_prover.rs) selects SDK `NetworkSigner::local` or `NetworkSigner::aws_kms` through `SP1_PRIVATE_KEY` / `SP1_KMS_KEY_ID`. Authorizes paid proving requests. | This is a service account, not a guest proof-result key. Audit the SDK graph in the devnet workstream. |
| SP1 deposit wallet (devnet workstream) | [`deposit.rs`](../../proofs/workers/sp1/src/cmd/deposit.rs) builds an L1 wallet using the selected local or KMS source for funding transactions. | Account for this funding authority separately from request authorization, even when configured to use the same account. A funding network is not evidence that the World Chain SP1 prover runs on mainnet. |

The shared [transaction signer](../../proofs/services/tx-signer/src/lib.rs) requires
exactly one local key or nonempty KMS key ID. KMS initialization/signing errors
propagate; it does not switch to local signing after failure. Native KMS protects
key extraction but does not itself enforce contract/selector transaction policy.
Selecting it is not equivalent to the proposed purpose-restricted enclave service.

On-chain [`NitroProofVerifier`](../../pkg/contracts/src/dispute/nitro/NitroProofVerifier.sol)
recovers the result signer from `keccak256(publicValues)` and asks the registry
whether it is active for the game's pinned image. Moving that key outside the
proof enclave would break this security model even if the signature bytes matched.
The host's placeholder verification mode is a development mode, not production
evidence of attestation validation.

## Dependency assessment: RUSTSEC-2026-0387

Static verdict: **needs review** for exploitability; affected locked versions and
local secret-signing paths are established. The [advisory](https://rustsec.org/advisories/RUSTSEC-2026-0387.html)
reports timing/microarchitectural leakage in operations intended to be constant-time;
its patched range is `>=0.7.6`. Actual leakage depends on the downstream operation,
compiler, CPU and an attacker's observations. No exploit, key recovery or deployed
exposure was demonstrated in this audit.

| Graph | Locked dependency / use | Consequence for remediation |
| --- | --- | --- |
| Root `Cargo.lock` | `crypto-bigint 0.5.5` via `elliptic-curve 0.13.8` / `k256 0.13.4`; local PBH and service-wallet signing. SP1 SDK 6.10.0 also brings Alloy 1.8.3 alongside World Chain's Alloy 2.5.0. | Assess each deployable and enabled signer. The devnet-only SDK may retain the old dependency after testnet/mainnet signing paths are fixed; track that separately rather than blocking the production fix on repository-wide version removal. |
| Measured Nitro lock | Same bigint/curve versions; enclave proof-result signing uses the local secret key. P-384 attestation verification operates on public data. | Fix the signing implementation inside the enclave; preserve computation binding. Review the complete Kona/revm/Alloy graph, including verification dependencies. |
| Measured SP1 lock (devnet only) | Same bigint version through an SP1-patched `k256 0.13.4` fork. | Do not replace the fork blindly: preserve zkVM patches and rebuild/verify guest measurements when those measured inputs change. This is not a testnet/mainnet remediation gate. |
| Nitro fuzz lock | Also contains bigint 0.5.5. | Include in eventual graph cleanup; its presence alone does not establish a deployed signing path. |

Native AWS KMS signing sends the digest to KMS; local RustCrypto recovery and
verification operate on the public signature/public key. That does not establish
private-key leakage through this local dependency for the KMS path. It also does
not protect the separate local PBH wallet or Nitro proof-result key.

The measured enclave's [`deny.toml`](../../proofs/measured/nitro-enclave/deny.toml)
explicitly ignores this advisory. A successful supply-chain check with that exception
is not evidence of remediation. Do not widen the exception or report the graph fixed
while affected versions remain.

Dependency feasibility check:

```sh
cargo update --locked -p crypto-bigint --precise 0.7.11 --offline
```

This fails because `elliptic-curve 0.13.8` requires `crypto-bigint ^0.5`, including
through Alloy 2.5.0. Cached manifests independently confirm this constraint;
the probe changed no lockfiles. `k256 0.14` uses a new curve/ECDSA API generation.
An additional direct dependency cannot override incompatible transitive requirements.

## Upstream status checked 2026-11-15

The advisory has been picked up upstream. Fixed RustCrypto releases exist, but the
current published downstream libraries and node release lockfiles still retain
affected dependency versions. Registry versions and release-tag manifests were
checked live; the older local dependency pins alone do not establish this status.

| Component | Verified upstream state |
| --- | --- |
| RustCrypto | Published `crypto-bigint 0.7.11`, `elliptic-curve 0.14.3` and `k256 0.14.0` can resolve a patched chain. The latest published 0.5 release remains 0.5.5. See the [advisory's fixed range](https://rustsec.org/advisories/RUSTSEC-2026-0387.html) and [registry](https://crates.io/api/v1/crates/crypto-bigint). |
| Alloy | Latest published signer crates are 2.5.0, still requiring k256 0.13; latest published `alloy-primitives` 1.7.3 also uses k256 0.13. [Issue 4445](https://github.com/alloy-rs/alloy/issues/4445) is open. |
| Alloy core migration | [PR 1147](https://github.com/alloy-rs/core/pull/1147) moves to k256 0.14 but remains open/unmerged. It does not by itself migrate Alloy's signer crates or SP1's SDK. |
| SP1 | Latest published SDK/zkVM is 6.10.0; the SDK requires k256 0.13.4. [PR 3031](https://github.com/succinctlabs/sp1/pull/3031), merged October 26, explicitly ignores this advisory pending elliptic-curve 0.14 migration. |
| Reth | Latest release v2.11.0, published November 11, still locks bigint 0.5.5 alongside patched 0.7.10. [Release lockfile](https://github.com/paradigmxyz/reth/blob/v2.11.0/Cargo.lock). [PR 28117](https://github.com/paradigmxyz/reth/pull/28117) explicitly added an advisory exception. |
| OP Reth | Latest release op-reth/v2.8.2, published November 10, still locks bigint 0.5.5 and 0.7.5, both below the patched range. [Release lockfile](https://github.com/ethereum-optimism/optimism/blob/op-reth/v2.8.2/rust/Cargo.lock). |

Lockfile presence is not proof that each dependency is compiled into every binary,
handles secrets, or is exploitable. These checks establish that simply selecting
the latest published releases is not a complete repository dependency-graph
remediation. That broader cleanup is distinct from fixing the signing paths active
on testnet/mainnet; SP1 is devnet only.

Assess the existing upstream curve migration and affected deployed signers before
choosing a private backport. Track the devnet SDK migration independently. A
temporary fork carrying reviewed upstream changes may be useful; a 0.5 arithmetic
backport remains a fallback whose applicability and constant-time properties need
independent review.

## PBH backend assessment

Alloy 2.5.0 already offers `Secp256k1Signer`: CLI parsing, public-key/address
derivation and private ECDSA signing use libsecp256k1. Its remaining k256 signature
conversion handles public signature bytes. Changing the PBH CLI/context fields
directly would avoid the selected affected secret route; parsing first as
`PrivateKeySigner` and converting afterward would still derive the address through
k256. This is an existing backend option, not a need to backport bigint arithmetic.

However, the replacement does not preserve an existing key-lifetime property.
The current k256 `SigningKey` (`ecdsa 0.16.9`) zeroizes its scalar on drop. Alloy's
`Secp256k1Credential` in 2.5.0 has no `Drop` implementation and holds a `Copy`
`secp256k1 0.30.0::SecretKey`; that key exposes explicit `non_secure_erase` but does
not call it automatically. A direct swap therefore removes automatic erasure of
retained PBH keys/clones. This is a source-confirmed regression, not evidence of
memory disclosure or proof that either backend erases every temporary copy.

Retain the current production backend until the replacement preserves this
property through a reviewed implementation. Do not add an unsafe memory-erasure
shim or silently accept a security tradeoff to clear the advisory. The
[PBH compatibility corpus](../../crates/builder/testdata/pbh-signing-v1.json)
and tests through `spend_nullifiers_tx` remain usable for that change and for the
separately reviewed restricted remote signer. They establish transaction
compatibility, not constant-time behavior or deployed remediation.

The [protected signing adapter](../signer-integration.md) now supports remote PBH
signing without constructing a host PBH private key or falling back to one.
Explicit local mode retains `PrivateKeySigner`; the affected dependency graph
and the other secret-signing roles below are not remediated by this adapter.

Other roles remain separate: proof-service local wallets and the measured Nitro
result key require their own assessment. Upstream Reth also converts the node's
network secret to `discv5::enr::CombinedKey` in `Discv5::start`; confirm actual
discovery-v5 configuration before claiming exposure. Disabling discovery v4 does
not by itself establish whether v5 is enabled. This node-identity route is not
addressed by changing the PBH wallet.

## Remaining acceptance gates

1. For testnet/mainnet, identify the deployed binaries and enabled signers, then
   remediate their affected secret-signing paths through a compatible upgrade or
   reviewed backport. SP1 SDK/guest migration is not a prerequisite. State explicitly
   whether acceptance covers active signing paths or the entire dependency graph.
2. Re-run advisory/dependency review per affected deployable and lockfile. Preserve
   local/KMS source selection, PBH transaction and proof-verification compatibility.
   Remaining devnet dependencies stay tracked; remove each advisory exception only
   when its covered consumers are resolved or explicitly reclassified with evidence.
3. When measured inputs actually change, follow [reproducible builds](reproducible-builds.md):
   run the affected standalone-workspace checks and review freshly generated
   measurements. Nitro changes require Linux x86_64 Nix EIF/PCR qualification;
   SP1 changes require pinned guest ELF/vkey qualification in the devnet workstream.
   Root workspace tests do not cover the standalone enclave executable.
4. For an affected proof deployment, follow the [proof release process](release.md):
   retain workers for games pinned to old measurements and obtain required image
   approval, signer registration and implementation activation before cutover.
   An unaffected SP1 deployment does not gate a node-only signing fix. Operational
   changes remain separate from this audit and are not authorized by it.

The dependency remediation remains open. This audit introduces no generic proof
signing endpoint, key migration, measurement change or on-chain role change.
