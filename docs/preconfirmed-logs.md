# Preconfirmed log API

This optional API exposes logs from locally validated Flashblocks before their
block becomes canonical. It provides provisional notifications and bounded
replay; it does not change `eth_getLogs`, consensus, confirmations, or finality.

**Billing-policy approval remains a prerequisite for billing use.** A
preconfirmed notification must not authorize an irreversible charge or weaken
the existing 12-block/finalization policy. A `canonical` event means inclusion in
the local canonical chain, not twelve confirmations, L1 finalization, or finality.

## Enable and discover

`--rpc.preconfirmed-logs` defaults to off and requires a valid
`--flashblocks.enabled` configuration. Add the flag to an existing configured
Flashblocks node; enable the `eth` namespace on each required transport:

```text
--flashblocks.enabled --rpc.preconfirmed-logs \
--http --http.api eth,net,web3 --ws --ws.api eth,net,web3
```

The equivalent feature environment variable is
`WORLD_CHAIN_PRECONFIRMED_LOGS=true`. Preserve the deployment's existing network
access controls and RPC authentication; this flag adds no authentication.

`op_supportedCapabilities` includes `worldchain_preconfirmed_logs_v1` when the
feed is enabled. Query methods are registered on enabled `eth` HTTP and WebSocket
transports; subscribe/unsubscribe are WebSocket only. With the feature disabled,
the capability is absent and the methods return JSON-RPC method-not-found.
Capability discovery confirms configuration, not feed freshness.

When HTTP and WebSocket share a listener, their standard namespace selections
must match. Subscriptions still require WebSocket: the shared listener rejects
HTTP subscribe/unsubscribe with the standard transport error `-32603`; distinct
HTTP listeners omit those methods (`-32601`).

## Query and subscribe

POST this JSON to the node's HTTP RPC endpoint (for example,
`http://127.0.0.1:8545`). The address is illustrative; replace it with the correct
chain's contract. The topic is
`Registered(uint64,uint64,uint256)`; all three arguments are indexed.

```json
{"jsonrpc":"2.0","id":1,"method":"worldchain_getPreconfirmedLogs","params":[{"address":"0x1111111111111111111111111111111111111111","topics":["0x34db382174d785a7e95b8e462fc059aa18422a3ed86677e4904f85fefbc1fab1"]},{"limit":100}]}
```

The second parameter is optional: `{ "cursor": "...", "limit": 100 }`. Without a
cursor, the result starts an atomic snapshot of the current provisional view.
Continue with its returned cursor and identical filter until `hasMore` is false;
subsequent queries replay changes after that snapshot, including changes made
while its pages were read. Advance the stored cursor only after processing a
whole response, including responses containing no matching logs.

Send this JSON on the node's WebSocket connection (for example,
`ws://127.0.0.1:8546`):

```json
{"jsonrpc":"2.0","id":2,"method":"worldchain_subscribe","params":["preconfirmedLogs",{"address":"0x1111111111111111111111111111111111111111","topics":["0x34db382174d785a7e95b8e462fc059aa18422a3ed86677e4904f85fefbc1fab1"]},{"limit":100}]}
```

The response supplies a subscription ID. Notifications use method
`worldchain_subscription`, with `params.subscription` set to that ID and
`params.result` containing either:

- `{ "type": "update", "result": <QueryResult> }`
- `{ "type": "error", "code": -32051, "message": "...", "canonical": <checkpoint> }`

The notification shapes above use placeholders for nested objects. A new
subscription without a cursor starts with a snapshot. Supply the last fully
processed query/notification cursor in the optional third parameter to replay
and then follow live changes on the same feed. The server drains paginated
results automatically; clients still process every update and retain its cursor.

Unsubscribe using the actual ID returned by the server, preserving its JSON type
(string or number):

```json
{"jsonrpc":"2.0","id":3,"method":"worldchain_unsubscribe","params":["<subscription-id>"]}
```

Filters support standard address and topic matching, including alternatives.
`fromBlock`, `toBlock`, and `blockHash` are unsupported; historical queries use
`eth_getLogs`.

## Results and lifecycle

`QueryResult` uses camelCase fields:

| Field | Meaning |
| --- | --- |
| `version`, `chainId`, `status` | API version `1`, chain ID, and feed state. Successful queries are `ready`. |
| `view` | Provisional `chainId`, `parentHash`, `blockNumber`, `blockHash`, `payloadId`, `flashblockIndex`, and `timestamp`; nullable. |
| `canonical` | Local canonical checkpoint `{number, hash}` for recovery; this is not a finalized checkpoint. |
| `events` | Ordered `{sequence, kind, view, logs, reason?}` changes. Each log contains standard RPC log fields plus `eventId`. |
| `cursor`, `hasMore` | Opaque continuation position and whether more pages are available. |
| `viewAgeMs` | Age of the validated source view, or null when unavailable. |

Custom numeric metadata uses JSON numbers; standard log fields retain their
usual Ethereum RPC encoding. Event views describe each change; the envelope view
may describe a later current view during replay.

| Event kind | Client action |
| --- | --- |
| `snapshot` | Establish the provisional set, accumulating all snapshot pages. |
| `preconfirmed` | Add newly validated logs; cumulative Flashblocks do not repeatedly add old logs. |
| `canonical` | Promote matching `eventId` entries to canonical inclusion; retain the approved confirmation policy. |
| `removed` | Retract provisional or reorged logs (`removed: true`); undo provisional effects. |
| `reset` | Discard the provisional set and reconcile canonical state before taking a fresh snapshot. `reason` explains the gap/reset. |
| `progress` | Advance the cursor/checkpoint even if no matching log changed. |

Treat `eventId` as a lifecycle identity: promotion or removal is a state change
for that event, not a second registration. Do not identify an event solely by
its provisional block hash, which can change as the block grows.

## Reconnect and recover

Cursors belong to one node process/feed generation and one filter scope. They
are not portable across replicas, restarts, or incompatible versions. Keep
requests for a cursor on the same backend; preserve the address/topic filter.
Changing `limit` is allowed. Cursors are opaque and are not authorization tokens.

Both query and subscription methods work on WebSocket. A direct connection to
one World Chain node uses that node's feed for both. Behind a proxy, verify
backend affinity separately: proxyd v4.34.1 routes HTTP-mapped methods through
its HTTP forwarding pipeline even when received over WebSocket. Those queries
can reach a different backend from the subscription on the same connection.

A subscription without a cursor supplies its own initial snapshot and drains
pagination on its selected backend. Cursor-bearing queries and handoffs from a
query to a subscription require explicit backend affinity or resynchronization;
sharing a WebSocket connection alone does not establish that affinity.

After a disconnect, resume with the last fully processed cursor when the backend
accepts it. A different backend, restarted feed or expired history requires the
recovery below. An ordinary disconnect may lose the terminal error notification,
so silence is never proof that no events occurred. Replayed lifecycle events
must be processed idempotently. History is bounded and has no time-retention
promise.

On a reset, expired/foreign cursor, or unavailable feed:

1. Stop provisional actions and retain the last durably reconciled canonical
   checkpoint; discard provisional state when resynchronization is required.
2. Once the feed is ready, take a fresh atomic snapshot, finish its frozen pages,
   and retain its cursor and canonical checkpoint. Buffer or replay subsequent
   events while reconciling; do not discard that continuation position.
3. Backfill the original filter through **that snapshot's canonical checkpoint**
   with bounded `eth_getLogs` ranges. Overlap the last reconciled range, verify
   chain ID and checkpoint hashes, and deduplicate canonical logs. Rebuild the
   provisional set from the snapshot and catch up from its cursor before
   enabling provisional actions. On history expiry, another reset, or reorg,
   repeat this reconciliation. The approved confirmation/finalization policy
   still selects what may trigger irreversible actions.

Backfilling first requires an additional backfill through the fresh snapshot's
checkpoint; otherwise a block sealing between the backfill tip and snapshot can
be missed. Canonical backfill and provisional replay must reconcile their overlap.

HTTP queries and subscription registration return JSON-RPC errors. An active
subscription sends a terminal `type: "error"` notification when possible, then
closes; backpressure or transport loss can prevent that notification.

| Code | Meaning and recovery |
| --- | --- |
| `-32601` | Feature/transport unavailable; check capability and configuration. |
| `-32602` | Invalid filter, cursor, or subscription kind; fix the request. |
| `-32050` | Initializing, stale, or unavailable feed; use canonical RPC and retry with bounded backoff. |
| `-32051` | Resynchronization required, including expired history/snapshot or another node/generation; backfill and take a new snapshot. |
| `-32052` | Resource/admission/delivery limit; reduce scope/page size or consumption lag and recover before retrying. |

Availability and resync errors include a canonical checkpoint in error `data`;
terminal notifications include `canonical`. Repeatedly retrying a malformed,
expired, or foreign cursor does not repair it.

## Bounds and rollout

Current defaults are: 5 seconds without a newly validated view before stale;
256 retained events / 8 MiB history; 32,768 logs / 8 MiB per view; 1,000 logs /
512 KiB per response; 8 retained paginated snapshots / 16 MiB total; 128 combined
address/topic alternatives; 8 concurrent queries and 64 active subscriptions
shared by the node's feed. Pages can be smaller than the requested `limit`.
These are implementation bounds, not a durability or delivery guarantee.

Subscriptions permit at most 1 MiB per notification, an 8-message connection
queue threshold, and a 2-second send deadline. A slow consumer is disconnected
instead of blocking validation or growing memory without bound. Feed failure
does not make standard canonical RPC a preconfirmed feed.

The API is default-off for staged qualification. **RPC proxy rollout remains
outstanding:** verify HTTP method routing/allowlists, WebSocket upgrade and
subscription notification/unsubscribe handling, same-backend cursor affinity,
reconnect/failover recovery, payload limits, timeouts, and backpressure through
the actual proxy before exposure. Configure HTTP routing for
`worldchain_getPreconfirmedLogs` and `op_supportedCapabilities`, without response
caching. Add `worldchain_subscribe`, `worldchain_unsubscribe`,
`worldchain_getPreconfirmedLogs`, and `op_supportedCapabilities` to the WebSocket
method whitelist. Cursor-bearing queries need backend affinity or explicit
resynchronization on backend changes, whether received over HTTP or WebSocket.
Keep subscription methods on the proxy's WebSocket forwarding path and verify
how HTTP method mappings affect queries received over WebSocket.
The [local pinned proxyd compatibility check](support/proxyd-preconfirmed-qualification.md)
passed for custom WebSocket forwarding, whitelist enforcement, and test-path
authentication with a mock backend; it does not qualify deployed routing or an
actual World Chain node behind the proxy.
Test on devnets, then Sepolia, before any mainnet rollout. Enabling the API is
separate from billing-policy approval;
neither deployment nor end-to-end proxy qualification is established by this
document. Roll back by disabling `--rpc.preconfirmed-logs` and returning clients
to canonical RPC.

## Operate

The existing node metrics recorder exports bounded `worldchain.preconfirmed.*`
metrics for Datadog collection. Track `publication_latency_seconds`,
`query_seconds`, `snapshot_age_seconds`, `view_age_seconds`, and the
`validated_views` rate for latency and fresh progress;
`source_lag`, `canonical_lag`, `resets`, `replay_expiry`, and `query_errors` for
recovery; and `active_subscriptions`, `queued_notifications`, `history_bytes`,
`subscription_rejections`, and `subscription_errors` for pressure and failures.
Metric labels contain only bounded reason/error categories.

An idle subscription enforces its freshness deadline even without a source
wakeup. Source-worker failure makes this API unavailable and emits a warning;
it does not stop the node or change canonical RPC health. A terminated worker
does not restart automatically: investigate the warning and restore the feed
with a normal node restart or keep the feature disabled. Qualify dashboards,
stale-feed alerts and rollback with the proxy before enabling consumers.
