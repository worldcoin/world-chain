# Upgrading to World Chain v2.9.0

World Chain v2.9.0 embeds op-reth v2.9.0 and the OP-patched Reth
`op-v2.7.0-1` maintenance release. World Chain, op-reth and core Reth have separate
version numbers. Deploy the World Chain image for World Chain's PBH and
Flashblock functionality.

## Follower pairing and rollback

The bundled `op-node v1.20.2-worldchain.1` is the only op-node locally paired
with this execution candidate. An RPC follower does not need the bundled
batcher, proposer, challenger or conductor. The [upstream op-node v1.20.2](https://github.com/ethereum-optimism/optimism/releases/tag/op-node%2Fv1.20.2)
contains World Chain network configurations, and the fork-only signing and
unsafe-block evidence features are optional for an ordinary follower. This
supports testing a vanilla op-node, but neither that pairing nor older partner
versions and mixed-version rolling upgrades have passed qualification. Use the
bundled op-node for the first canary; test any other exact binary/version with
the candidate execution client before a fleet rollout. Unsafe-block evidence
forwarding requires the fork op-node's evidence API.

The Reth database version marker remains `2` between the v2.8.3 and v2.9.0
pins. That marker does not establish that the older binary can reopen a
directory written by the new binary, especially for archive nodes, the
independent storage-v1/v2 layouts and proof-history data. Direct downgrade
remains unqualified.

For a v2.8.3 follower canary, record both binary digests, effective flags and
storage layout; stop the old pair cleanly and retain an untouched replica or a
consistent pre-upgrade copy of the complete execution and op-node data. Include
MDBX, static files, RocksDB and any proof-history store in the copy. Start the
candidate pair on a separate copy. Check derivation, advancing unsafe/safe and
finalized heads, historical state/proofs, transaction forwarding, RPC latency
and errors before routing traffic. If the canary fails, stop both candidate
processes and return traffic to the untouched replica. To reuse the same host,
restore the entire pre-upgrade copy before restarting the old binaries. Do not
start the old binary on data modified by the RC until a separate reverse-open
test has passed. Measure snapshot restore time for the provider's own storage
and traffic setup before scheduling a fleet rollout.

## RPC compatibility

- `--rpc.gascap` now also caps `eth_estimateGas`. Its default is 50,000,000.
  Set an explicit budget on endpoints that must estimate larger transactions.
  Account for access policy, concurrency and the block and protocol gas limits.
  Each `eth_callMany`, `trace_callMany` or `debug_traceCallMany` request
  accepts up to 1,024 calls that share the cap.
- `--rpc.max-tracing-requests`, `--rpc.max-blocking-io-requests` and
  `--rpc.proof-permits` require positive values. A configured zero prevents startup.
  Remove zero overrides to use the defaults. Restrict method access at the
  listener or gateway if the zero was intended to disable expensive requests.
- Queries for pruned state now return JSON-RPC error `4444`, with the requested
  and earliest available block numbers. Update consumers matching the former
  internal-error code or message. Check optional proof-history overrides separately.
- `trace_callMany` without a block and `trace_filter` without bounds default to
  `latest`. Specify `pending` or historical bounds when those are required.
- The default prestate tracer omits accounts that were empty before execution,
  including accounts funded only by fees. Disabling code output still retains
  pre-existing accounts whose only nonempty field was code. The bounded
  projector follows the same filtering while retaining its traversal limits.

Run these checks against the old and new binaries with the same fixture and
effective gateway configuration. The expected RC behavior reflects the pinned
implementation. Record the wire responses from the deployment you qualify.

| Request | v2.8.3 | v2.9.0 RC |
| --- | --- | --- |
| `eth_estimateGas` for a call that needs more than the configured RPC gas cap but fits the block and protocol limits | The RPC gas cap does not bound the search. | The gas cap bounds the search. The request fails if the call cannot complete within it. |
| `eth_getBalance` at a known pruned block | Internal error. | Error `4444` with the requested and earliest available blocks. |
| `eth_getProof` for an available historical block | Baseline historical proof. | Verify the account and storage proofs against the canonical state root. Requests for more than 1,024 storage keys return invalid params (`-32602`) on both proof paths. |
| `trace_callMany` without a block | Defaults to `pending`. | Defaults to `latest`. Pass `pending` explicitly to retain the old scope. |
| `trace_filter` without `fromBlock` or `toBlock` | Defaults to block 0 through the latest block, subject to normal range and history checks. | Defaults to the latest block only. Set both bounds for an explicit range. |

For JSON-RPC batches, check the request count, body and response limits on the
actual gateway route. The node's HTTP and WebSocket size defaults are
`--rpc.max-request-size=15` and `--rpc.max-response-size=160` (MiB). The
1,024-key proof limit applies to each proof request.
Check large responses and trusted publisher identities before enabling
[Flashblock evidence](flashblock-evidence.md) or
[unsafe-block evidence](unsafe-block-evidence.md).

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
