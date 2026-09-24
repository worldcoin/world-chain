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

## Further reduction candidates

The clearest next change is to share the range runner between
`sp1-programs/range-utils/src/lib.rs::run_range_program` and
`nitro-enclave/src/enclave.rs::run_full_range_program`. They repeat setup, execution and
public-value conversion. A backend-neutral function in the shared client can return a
`Result`; SP1 can fail the guest on error and Nitro can attach request context. Keep
backend-specific proof commitment and signing outside that function.

The local `WitnessExecutor` trait has one implementation and its schedule-free `run`
wrapper has no callers in the inspected repository. Simplifying that interface may remove
boilerplate, but does not remove a major protocol algorithm. Preserve the host witness
execution caller as well as both proof backends if doing this cleanup.

No claim is made here that the wider World witness/core or aggregation code is minimal.
Replacing those types or algorithms requires a separate comparison of serialization,
validation and public-statement semantics.

## Verification and update process

The source refactor passed 11 shared-client tests, 64 Nitro native tests and native SP1
workspace compilation. Linux Nitro dependency resolution also passed. Native Nitro tests
on macOS do not compile the Linux-only enclave execution path; native SP1 checks do not
execute a zkVM guest. These results do not establish complete proof equivalence.

When updating the Kona pin, align the relevant measured manifests, review upstream changes
and features, refresh each consumer lockfile, run regression and representative proof
execution tests, and rebuild both artifacts. Follow [reproducible builds](reproducible-builds.md)
for EIF/PCR and SP1 measurement verification. Changed source or dependency resolution is
not evidence that committed measurements are current.
