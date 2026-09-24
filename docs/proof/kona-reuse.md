# Kona reuse and World proof adapters

The shared client in `proofs/measured/kona-client` serves SP1, Nitro, and host witness
execution. It imports upstream implementations where their interfaces support World’s
execution configuration. It does not replace World’s entire proof program with Kona’s SP1
programs.

## Dependency boundary

Kona and `kona-sp1-client-utils` are pinned to Optimism commit
`96ffbb2a94f19886fe7e27c45f3310e64ccd18b3`. The manifests, rather than the upstream
`develop` branch, are the source of truth for the selected revision.

Before PR #1165, the client already depended on core Kona crates at this revision, but
maintained local copies of several helpers. The PR adds `kona-sp1-client-utils` at the
same revision for its crypto provider and cycle metrics. It does not bump Kona or import
an SP1 SDK/runtime into Nitro. This dependency enables additional serde/tracing features;
the consumer lockfiles remain part of the measured build boundary. KZG resolves to 0.2.8.

## Removed implementation copies

| Responsibility | Upstream implementation now used | Local responsibility retained |
| --- | --- | --- |
| Safe-head output-root lookup and version validation | `kona_proof::sync::fetch_safe_head_hash` | Re-export and regression tests |
| Sync-start validation and cursor/provider initialization | `kona_proof::sync::prepare_derivation` | Load boot inputs, adapt the return value and preserve starting height |
| Derivation/execution loop, including retry and end-of-source behavior | `kona_driver::Driver::advance_to_target_with_metrics` | Invoke the driver with World’s executor and validate its result |
| KZG point-evaluation crypto adapter | `kona_sp1_client_utils::precompiles::CustomCrypto` | Install it into revm; retain valid/invalid-opening tests |
| Per-phase SP1 cycle markers | `kona_sp1_client_utils::metrics::CycleTrackerDriverMetrics` | Pass the collector to the driver; retain whole-range markers |
| V0 output-root encoding and hashing | `kona_protocol::OutputRoot` | Preserve `OutputRootWitness` serialization and field names |
| Preimage storage, validation and oracle implementation | `kona_sp1_client_utils::witness::preimage_store` | Public re-export and wire-compatibility tests |
| Blob input container | `kona_sp1_client_utils::witness::BlobData` | Embed it in World’s schedule-bearing witness |
| Blob verification and lookup | `kona_sp1_client_utils::BlobStore` | Re-export; regression tests for valid, reordered and malformed inputs |
| Witness validation flow | `kona_sp1_client_utils::witness::WitnessData` | Implement the trait for `WorldRangeWitnessData` |
| Range-vkey word/byte conversion | `kona_sp1_client_utils::types::u32_to_u8` | Re-export and known-vector test |


Two concrete drift examples motivated the change. Our copied safe-head helper omitted
output-version validation even though the pinned Kona helper already rejected nonzero
versions. Our copied KZG adapter previously accepted `Ok(false)` from `kzg-rs`; the pinned
upstream adapter already rejected it. PR #1164 fixes the local KZG behavior; PR #1165 then
replaces that implementation with the upstream provider while retaining regression tests.

Updates to the pin now bring changes to these imported implementations directly. This is
not an automatic update policy: API compatibility, feature selection, proof behavior and
artifact measurements must still be checked.

## Code still maintained locally

- **World EVM factory and schedule:** `precompiles/factory.rs` retains its construction and
  post-execution hooks, including overlap with the standard OP factory. This is the local
  extension point for World execution rules; it was deliberately left unchanged.
- **Executor integration:** `executor.rs` selects the World factory, installs crypto,
  constructs the upstream executor/driver and validates the returned root and height.
  Its Ethereum DA pipeline constructor overlaps with upstream setup code. Upstream’s SP1
  witness runner hardcodes its own factory and cannot directly accept World’s schedule.
- **Claim postconditions:** root and height validation remain local. Upstream’s SP1 witness
  runner also implements them, but its generic driver can return an earlier safe head
  after `EndOfSource`. Calling that driver successfully does not prove the originally
  requested height was reached. The upstream runner’s height helper is private.
- **Proof input/output:** World witness serialization, starting-height propagation, fork
  configuration binding and transition public values remain World responsibilities.
- **Proof programs and enclave:** World aggregation, recursive proof checks, Nitro request
  handling, attestation and signing are not replaced by importing Kona utilities.

The refactor preserves World’s EVM factory. It adopts upstream’s zero-step claim handling
and header/first-transaction block-info extraction. The old custom periodic progress logs
are removed; upstream logs and cycle measurements remain.

## Witness reuse follow-up

The preimage store, blob data, blob store, witness trait and vkey conversion identified in
our source review below now all delegate to the same pinned upstream utility crate.
The local blob provider and its error enum have been removed. World’s schedule-bearing
witness and public ABI remain local; the EVM factory is unchanged.

Compatibility tests deserialize a nonempty legacy rkyv witness into the upstream-backed
World type and back again, and check serde output for the component types. Real constant
polynomial blob vectors exercise verification and reversed request order; rejection tests
cover invalid proofs, missing blobs and mismatched empty inputs.

Upstream invalid blob inputs panic rather than produce the former `BlobStoreError`.
SP1 fails the guest execution. Nitro runs each connection in `tokio::spawn`, and the
inspected measured build uses the default unwind strategy: such a panic ends the request
task and drops its connection, rather than producing `EnclaveResponse::Error`. No local
catch-and-reimplement validation layer is added. Do not change the enclave to panic-abort
without reassessing this boundary. Process/task behavior here is based on source/build
configuration inspection; an actual EIF request test remains a release validation step.

## Historical reuse inventory at `e19990dd`

The candidate statuses below record the investigation before the witness reuse follow-up;
the five witness/conversion candidates have since been implemented as described above.

This review covers all 21 Rust source files in the measured core, Kona client and three
SP1 programs, plus the SP1 request/vkey glue relevant to their interfaces. It compares the
pinned Optimism source, not an unpinned latest branch. It is not an exhaustive review of
Nitro attestation, host services or upstream security. Duplicated TEE/SP1 orchestration is
acceptable; the priority is removing locally maintained upstream algorithms.

| Local code | Upstream counterpart | Assessment |
| --- | --- | --- |
| `core/witness/preimage_store.rs` | `kona_sp1_client_utils::witness::preimage_store::{PreimageStore, check_preimage}` | Strong reuse candidate. Validation, insert-only storage and oracle implementations match. Upstream makes the map private; no external direct map accesses were found in this repository. Check rkyv/serde compatibility before replacing the type. |
| `core/witness/mod.rs::BlobData` | `kona_sp1_client_utils::witness::BlobData` | Same three fields and derives. Re-export candidate; test serialized witness compatibility. |
| `core/oracle/blob_provider.rs` | `kona_sp1_client_utils::BlobStore` | Important behavior divergence; see below. Upstream is not a behavior-neutral replacement. |
| `core/witness/mod.rs::WitnessData` | upstream `WitnessData` | Can potentially reuse once underlying stores/types align, implementing the trait for World’s schedule-bearing witness. Upstream blob construction panics; our current path returns an error. |
| `core/types.rs::u32_to_u8` | upstream `types::u32_to_u8` | Same big-endian conversion; direct re-export candidate with existing vector test. Small maintenance benefit. |
| `core/boot.rs` | upstream `boot.rs` | Retain World ABI and hash wrapper: World includes the starting height and Tropo/Strato hash inputs. Upstream’s hash helper only accepts `RollupConfig` and panics on serialization failure; it does not support our generic flattened input. SHA-256 and JSON serialization already come from dependencies. |
| `core/range.rs` | Kona schedule/spec types | Retain World schedule representation and fork extension; not an unchanged upstream type. |
| `core/artifacts.rs`, core module exports | upstream proof/boot types | World statement wrappers and exports, no additional protocol implementation to replace. |
| Kona client sync, driver, crypto, metrics and output root files | upstream APIs listed above | Already delegate; tests and wrappers remain. |
| Kona client executor and factory | upstream witness executor and OP EVM factory | Deliberate local customization/integration. Height check is private inside upstream’s hardcoded runner. Preserve factory as requested. |
| Kona client witness/module exports | World core exports | No algorithm to replace. |
| `sp1-programs/range-ethereum`, `range-utils` | upstream range entrypoint/runner | World witness, schedule and public-value integration. SP1 IO and Kona execution already delegated. Do not replace with the upstream boot statement. |
| `sp1-programs/aggregation` | upstream aggregation binary | Real algorithm overlap remains, but no reusable parameterized header-chain/aggregation helper was identified at the pin. World adds height continuity, carries its range key as input, changes output ABI and omits prover address. Upstream embeds a fixed range key and uses its own statement. Preserve World checks; recursive verification already calls `sp1_lib`. |
| SP1 request/vkey glue | SP1 SDK | World request/serialization wrappers and key plumbing; actual proving/key generation already delegated. The conversion helper above is the identified reusable duplicate. |

Upstream evidence at the pin: [preimage store](https://github.com/ethereum-optimism/optimism/blob/96ffbb2a94f19886fe7e27c45f3310e64ccd18b3/rust/kona/sp1/crates/client/src/witness/preimage_store.rs),
[witness types](https://github.com/ethereum-optimism/optimism/blob/96ffbb2a94f19886fe7e27c45f3310e64ccd18b3/rust/kona/sp1/crates/client/src/witness/mod.rs),
[blob store](https://github.com/ethereum-optimism/optimism/blob/96ffbb2a94f19886fe7e27c45f3310e64ccd18b3/rust/kona/sp1/crates/client/src/oracle/blob_provider.rs),
[aggregation](https://github.com/ethereum-optimism/optimism/blob/96ffbb2a94f19886fe7e27c45f3310e64ccd18b3/rust/kona/sp1/programs/aggregation/src/main.rs).

### Blob-store divergence

Pinned upstream `oracle/blob_provider.rs` checks all three input vector lengths before
batch verification and serves blobs by requested versioned hash (`position` plus
`swap_remove`). Our copy reverses the witness vector, pops positionally and drops an item
when its hash differs. Out-of-order requests can therefore lose available blobs and return
fewer than requested. Our constructor also lacks upstream's explicit count checks;
`kzg-rs 0.2.8` returns early for zero blobs before checking the other lengths.

Two temporary tests against the local blob-provider source reproduced both the empty
out-of-order result and acceptance of zero blobs with a nonempty commitment list. The
production file was restored after the probes. These are correctness/validation differences,
not a demonstrated accepted invalid proof. Upstream includes regression tests for reordered requests, missing hashes
and the zero-blob count mismatch. Its rejection behavior is panic-based, while World's
constructor uses `TryFrom` and structured errors. Choose the error behavior explicitly
before reuse; do not hide an upstream panic behind a supposedly fallible adapter.

Recommended order: reuse preimage store, blob data and vkey conversion after serialization
compatibility tests; then adopt upstream blob/witness handling if its fail-closed behavior
is acceptable for both backends. If structured errors are required, retain that small local
boundary and document the remaining blob-store maintenance instead of claiming full reuse.
Importing utilities into core broadens that crate's dependency graph and requires consumer
lockfile/measurement review, even though the final proof backends already use the utility
crate through the Kona client. No upstream changes are required for the first group.

## Verification and update process

The witness follow-up passed 12 core tests, 11 shared-client tests, 64 Nitro native tests and native SP1
workspace compilation. Linux Nitro dependency resolution also passed. Native Nitro tests
on macOS do not compile the Linux-only enclave execution path; native SP1 checks do not
execute a zkVM guest. These results do not establish complete proof equivalence.

When updating the Kona pin, align the relevant measured manifests, review upstream changes
and features, refresh each consumer lockfile, run regression and representative proof
execution tests, and rebuild both artifacts. Follow [reproducible builds](reproducible-builds.md)
for EIF/PCR and SP1 measurement verification. Changed source or dependency resolution is
not evidence that committed measurements are current.
