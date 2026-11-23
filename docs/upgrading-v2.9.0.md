# Upgrading to World Chain v2.9.0

World Chain v2.9.0 embeds op-reth v2.9.0 and the OP-patched Reth
`op-v2.7.0-1` maintenance release. World Chain, op-reth and core Reth have separate
version numbers. Deploy the World Chain image for World Chain's PBH and
Flashblock functionality.

## RPC compatibility

- `--rpc.gascap` now also caps `eth_estimateGas`. Its default is 50,000,000.
  Set an explicit budget on endpoints that must estimate larger transactions;
  account for access policy, concurrency and the chain's current gas limit.
  Existing reader configurations using 500,000,000 retain that budget.
- `--rpc.max-tracing-requests`, `--rpc.max-blocking-io-requests` and
  `--rpc.proof-permits` require positive values. A configured zero prevents startup.
- Queries for pruned state now return JSON-RPC error `4444`, with the requested
  and earliest available block numbers. Update consumers matching the former
  internal-error code or message. Check optional proof-history overrides separately.
- `trace_callMany` without a block and `trace_filter` without bounds default to
  `latest`. Specify `pending` or historical bounds when those are required.
- The default prestate tracer omits accounts that were empty before execution,
  including accounts funded only by fees. Disabling code output still retains
  pre-existing accounts whose only nonempty field was code. The bounded
  projector follows the same filtering while retaining its traversal limits.

## Resource defaults

RPC cache defaults decrease from 5,000 to 1,000 blocks and from 2,000 to 500
receipt blocks. New `--rpc-cache.max-blocks-bytes` and
`--rpc-cache.max-receipts-bytes` limits default to 2 GiB and 1 GiB, and
`--rpc-cache.idle-timeout` defaults to one hour. Measure cache misses, latency
and retained memory before changing these limits; restoring counts alone does
not remove byte limits.

The engine sender-recovery cache is enabled by default and can be disabled with
`--engine.sender-recovery-cache=false`. The configurable
`--engine.backfill-threshold` retains the previous value of 32. World Chain's
Flashblock access-list configuration and state-masking prohibition are preserved.

## Qualification and recovery

Use the existing CI and gated release workflow. Before promotion, record the
candidate SHA, binary version, multi-architecture image digests and paired
consensus-client identity. Qualify PBH/deposit execution and settlement,
Flashblock BALs, cancellation, pending and canonical RPCs, gas-cap boundaries,
reorgs, proof-history reads and enabled evidence APIs on the actual candidate.

Promote progressively through the approved development and staging environments,
then a production reader canary before builders. Inspect effective flags and
init/snapshotter image overrides as well as environment image defaults. Rehearse
restart, snapshot restore and rollback using copied persistent data; an unchanged
database version is not sufficient proof of downgrade compatibility.

This node upgrade leaves the separately pinned measured SP1/Nitro workspaces and
their measurements unchanged. It introduces no World Chain genesis, hardfork,
contract or consensus-client deployment change. A merged source PR does not
establish artifact publication, deployment or feature activation.

See the [upstream release notes](https://github.com/ethereum-optimism/optimism/releases/tag/op-reth%2Fv2.9.0)
and [World Chain proof-build boundaries](proof/reproducible-builds.md).
