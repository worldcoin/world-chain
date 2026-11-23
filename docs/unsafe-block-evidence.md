# Unsafe-block evidence through the execution-client RPC

The execution client forwards two read-only methods to its paired op-node:

- `optimism_unsafeBlockEvidenceCapabilities`, with no parameters.
- `optimism_getUnsafeBlockEvidence`, with one exact 32-byte block hash.

Forwarding defaults off. Configure
`--rpc.unsafe-block-evidence-url http://127.0.0.1:9545` (or
`WORLD_CHAIN_UNSAFE_BLOCK_EVIDENCE_URL`) on the execution client and enable
`--rpc.unsafe-block-evidence` on its P2P-enabled op-node. This requires the
bundled World Chain op-node's evidence API. Methods are installed on
HTTP/WebSocket transports serving `eth`, excluding IPC and authenticated Engine
RPC. Opt-in applies to every such transport: inspect both listeners and their
existing network/proxy access policies. No new public endpoint or blanket
authentication requirement is implied. Do not add generic op-node passthrough.

The URL must identify this replica's paired op-node, such as a loopback sidecar,
not a load-balanced service containing unrelated caches. Each call makes one
upstream request, with no retries, redirects or environment-proxy routing.
There is no automatic failover: return unavailability for the requested hash.
An operator can deliberately select another qualified pair and query that same
hash; this forwarder never substitutes another hash or a newer head.

## Contract and limits

The original JSON result is preserved, including version, hexadecimal SSZ
payload/signature bytes and extension fields. The execution client validates the
JSON-RPC envelope and returned block identity; it does not reconstruct or re-sign
evidence. Consumers independently verify chain, historical signer, block version
and immutable hash. Signed origin does not establish validity or finality.

Upstream JSON-RPC code, message and error data are preserved, including `-32080`
with `blockHash` and `reason` (`notRetained`, `expired`, `evicted`). An older or
disabled upstream's `-32601` is preserved during capability discovery. The
execution client does not claim upstream support independently. Local transport,
HTTP, deadline, admission, response-size and invalid-response failures return
`-32081` with a bounded `reason`, affecting this optional API alone.

Each instance admits two concurrent upstream calls without a queue. Additional
calls return `busy`. Connect timeout is one second; the complete upstream
operation has a five-second deadline. Dropping the forwarding future cancels
owned upstream I/O and releases admission. A client disconnect does not always
make jsonrpsee drop an ordinary handler immediately; the deadline bounds any
remaining work. Cancellation cannot undo a synchronous op-node payload copy.

The upstream download/envelope limit is 24 MiB, including chunked responses.
Parsing/serialization need additional bounded copies, so this is not a total
process-memory budget. Startup requires `--rpc.max-response-size` of at least
24 MiB. This fits the maximum 10 MiB signed frame encoded as roughly 21 MB of
JSON, with headroom. The regression sends a synthetic maximum result through
both HTTP hops and compares the original JSON, including signed-byte fields.

Upstream admission ends before downstream draining. Route owners must separately
bound outstanding responses, slow-reader buffers, request/byte rates, batch
amplification and write/idle timeouts. Allow at least 24 MiB for a complete
response at every layer. Outer timeouts must allow the five-second upstream
budget plus transfer; a five-second proxy timeout races the upstream deadline.
Disable backend retries and backend-group failover independently.

Collect `rpc.unsafe_evidence.requests_total` (fixed method/outcome labels) and
`rpc.unsafe_evidence.response_bytes` / `rpc.unsafe_evidence.duration_seconds`
from Prometheus into Datadog, alongside
proxy rejection, latency, memory and egress metrics. Payloads, hashes and upstream
URLs are not logged by the forwarder. Qualify disconnected callers and downstream
draining as separate cases before broader exposure.

## Qualification

Proxyd and RPC Edge configuration belongs to their owners. The integration
contract is limited to these two method names, unchanged result/error data,
complete 24 MiB response support and no automatic replica retries. This change
does not configure or activate a proxy route.

Qualify local/devnet, then Sepolia, against exact World Chain/op-node source
revisions and immutable image digests. Exercise complete maximum responses,
missing/expired/evicted hashes, mixed versions, malformed selectors, unsupported
methods, outages, concurrent capture/lookups, overload, disconnected/slow callers
and loss of the paired backend on the consumer's actual route. Measure heap,
CPU, egress, outstanding responses and capture latency. Record the effective
topology/access policy for devnet, Sepolia/testnet and mainnet separately.
Source merge, artifacts and runtime qualification remain separate. Roll back
by removing the forwarding URL and the two proxy method mappings.
