# Protected World Chain signing

[BLO-1335](https://linear.app/worldcoin/issue/BLO-1335) tracks these adapters.
Pin the host signer to `97bf2eeaf0750abf8e1cb280c19b384e3ef11478`, which includes
[publication bounds PR 32](https://github.com/worldcoin/kms-tee-signer/pull/32),
[batcher cancellation PR 33](https://github.com/worldcoin/kms-tee-signer/pull/33), and
[shared body admission PR 34](https://github.com/worldcoin/kms-tee-signer/pull/34).
The [V1 contract](https://github.com/worldcoin/kms-tee-signer/blob/97bf2eeaf0750abf8e1cb280c19b384e3ef11478/docs/signer-protocol-v1.md)
and [runtime handoff](https://github.com/worldcoin/kms-tee-signer/blob/97bf2eeaf0750abf8e1cb280c19b384e3ef11478/docs/integration-handoff.md)
are the compatibility baseline.

## Roles and wire compatibility

| Role | Outbound signer method | Required result validation |
| --- | --- | --- |
| Grant authorizer | `teekms_signFlashblockAuthorizationV1` | Ed25519 over BLAKE3 of payload ID, little-endian timestamp and publisher key; verify the independently pinned authorizer key |
| Publisher | `teekms_signFlashblockPublicationV1` | Ed25519 over BLAKE3 of the exact message RLP followed by grant RLP; verify the independently pinned publisher key |
| PBH wallet | `eth_signTransaction` | Canonical signed type-2 bytes, every original unsigned field, low-S signature and independently configured recovered address |

These are outbound operations, not new public World Chain RPC methods.
The node normally receives authorizer grants from rollup-boost. `GrantSigner`
is available for conformance/integration; protected node mode does not create or
override grants. Payload/start/stop encodings and existing signature preimages
are unchanged. Existing Ed25519 preimages contain no chain ID: enroll separate
network keys, identities and policies. Peer identity keys and proof-result keys
remain separate; see [proof signing boundaries](proof/signing-boundaries.md).

## Configuration

Local signing retains the existing flags. An enabled builder requires an explicit
local key or remote configuration. Nodes without either hold no PBH signer and
fail explicitly if asked to sign a nullifier transaction. There is no embedded
default PBH private key or fallback after a remote failure.

Protected mode is enabled by `--builder.signer-config`. To publish Flashblocks,
also supply `--flashblocks.signer-config`:

```sh
world-chain node --builder.enabled \
  --builder.signer-config /run/secrets/pbh-signer.json \
  --flashblocks.enabled \
  --flashblocks.signer-config /run/secrets/publisher-signer.json
```

For a custom chain, additionally configure its explicit authorizer verifying key.
Protected mode rejects local PBH/publisher keys, authorizer overrides and
force-publish, including conflicting environment variables. Remote PBH can run
with Flashblocks disabled or receiving only. A remote publisher requires remote
PBH signing. Receive-only nodes need no publisher signing credentials.

PBH JSON:

```json
{
  "connection": {
    "endpoint": "https://signer.example.invalid:8443/",
    "tls": {"ca": "ca.pem", "certificate": "pbh.pem", "privateKey": "pbh.key"}
  },
  "expectedAddress": "0x<independently-approved-PBH-address>"
}
```

Publisher JSON uses the same `connection` structure with its own client
certificate/key, plus `keyId` and `expectedPublicKey`, each a `0x`-prefixed
32-byte value. Paths are resolved relative to the JSON file. Unknown/duplicate
fields, oversized files, invalid endpoint/keys and missing/malformed mTLS material
fail startup. No credentials or private-key contents are logged. TLS credentials
are host-side client authentication, not exports of the protected chain keys.

TLS 1.3 and HTTP/1.1 POST `/` are required, with explicit CA/hostname verification
and separate client identities. No redirects, compression, automatic signing
retries, downgrade or local fallback. `health_status` means protocol reachability;
it does not prove signing authorization, custody or execution correctness. Signer
availability is not a new process-liveness dependency; failures affect the owning
build/publication operation.

## Bounds and lifecycle

| Boundary | Limit |
| --- | --- |
| Complete Flashblock frame, including outer RLP/grant/signature | 16 MiB; the existing 8 MiB threshold remains a warning |
| Decoded message / authorization RLP | 16 MiB / 256 bytes |
| Complete publication JSON body | 32 MiB + 4,096 bytes |
| Signing operation | Earlier of the original caller deadline and three seconds |
| Shared client / PBH bridge | One admitted operation, no signing queue |
| Node control intents | At most two tasks: active control plus a cancellable handover |
| Signer runtime body admission | Two bodies above 4 KiB, including at most one above 1 MiB, retained through output |

Complete frame size is checked before remote publication signing. No truncation
or splitting. JSON requests/results and HTTP framing are independently bounded.
Cloned clients share admission. Caller cancellation drops the in-flight HTTP
connection. A subsequent admitted request waits within its existing deadline for
that connection's aborted driver to exit before opening another socket. Control
handover can wait for previous cancelled I/O to drain; it does not resend a signing
request or reset its budget.

### PBH construction and deadlines

The builder retains nonce selection from its in-progress EVM, chain/entry point,
zero value, HashSet-derived nullifier order, ABI calldata, gas
`100000 + 20000 * count`, and equal fee caps from the block's base fee.
`PbhWallet` selects the backend once. Remote responses are fully verified before
extracting the signature for the original transaction; the signer never chooses
a nonce, submits a transaction or executes the builder's EVM state.

`PbhBridge` runs from the existing blocking build worker, uses a bounded async
handoff/reply and checks cancellation before admission and after receiving a
result. It never nests another Tokio runtime entry. Admission remains held until
the async operation exits. Finalization still allows mandatory nullifier spending;
cancellation discards the result. Missing/expired deadlines and signer failures
fail the affected build attempt.

Flashblock jobs register their exact absolute deadline. Basic Reth jobs receive a
conservative budget beginning before parent lookup, using Reth's same configured
`deadline + min(time_until_timestamp, 3 * deadline)` rule. Both retain that budget
through the existing payload resource leases, including detached workers. An
active payload ID cannot be registered again to extend older work's budget.
Non-Flashblock parent lookup time therefore counts against the signing budget,
which can expire earlier than Reth's internal timer. There is no fresh per-attempt
budget. The default 200 ms publishing interval is not a three-second latency claim.

### Flashblock publishing and handover

Preparation and state fencing run synchronously; remote I/O runs outside P2P locks
and network polling. Start grants no clearance before verified completion. Stop
and peer handover revoke clearance before signing their notification. Handover
and publication signing failures fence broadcasting and switch the job to
execution-only building, with failure telemetry. PBH signing remains mandatory;
its failures still fail the affected build attempt. A missing stop notification
never restores clearance. The existing two-second peer handover fallback remains unchanged.

Clearance waits persist across job polls, and the publication timer is armed even
while clearance is pending, so successful signing wakes the job without a build tick.

A publishing lease binds a payload job to its generation and deadline. Signing
checks the exact grant, publisher, payload and current state. Completion rechecks
cancellation, generation, payload, grant and budget, including at the final cache
commit/broadcast boundary. A new intent invalidates old results even if it reuses
the same grant. Dropping or finishing a protected job fences only its own generation.
The preconfirmation cache is not mutated by a fenced result.

Only one publication future is retained per job. It is awaited before committing
the frame/index or starting work on its precommitted state. Dropping that future
cancels its RPC. Local and receive-only modes preserve their existing operation.
When `getPayload` resolves a remote job, cancel optional publication and clearance
and return the already-executed full block, even while start/handover clearance is
pending. Preserve its committed prefix without inserting a Flashblock into the
cache or broadcasting it. At the original job deadline, cancel signing and fence
publishing; never commit a late signature to recover a final frame.
`worldchain.remote_signer` records request/failure/in-flight/duration metrics by
role; `flashblocks.p2p.publishing_signing_failed`, `publishing_control_stale` and
`publishing_control_invalid` expose failed/discarded publication work.
`flashblocks.p2p.publishing_disabled` counts jobs entering execution-only mode.

## Verification and rollout

Frozen signing/evidence corpora are under `crates/primitives/testdata/`.
`crates/builder/testdata/pbh-signing-v1.json` covers both L2 networks, account
nonce/fee boundaries and the actual construction path. The separate runtime
corpus `crates/signer/testdata/pbh-runtime-v1.json` uses the harness's public
scalar two; its SHA256 is
`c1aab29feee9ffcd797d97f0712d61985a7f6d10fa1b477fd4b5b8c020a483a7`.
The original scalar-one corpus is not an enrolled runtime identity.

Offline tests cover configuration exclusion, exact encodings, limits, stale and
expired results, cache fencing and budget ownership. Explicitly ignored mTLS tests
exercise protected CLI/configuration loading, the production `spend_nullifiers_tx` path, payload jobs and P2P publishing handler,
including finalization/cancellation, denial, rapid handovers, a held signed response
cancelled by stop, same-grant generation changes and complete 16 MiB frames.
Job regressions cover build-only handover/denial, resolving a built block while
clearance or signing is pending, and deadline cancellation without cache publication.
Build the pinned signer example with `flashblocks,transactions`, then set its binary
path and exact revision before running:

```sh
cargo nextest run --locked -p world-chain-signer -p world-chain-builder -p world-chain-cli \
  -p world-chain-p2p -p world-chain-payload --run-ignored only --test-threads 1 \
  -E 'test(/actual_role_clients|actual_client_signs_complete|production_nullifier_transaction|remote_controls_payload|protected_startup_loads|remote_job/)'
```

`WORLD_CHAIN_SIGNER_HARNESS_BINARY` identifies the disposable executable;
`WORLD_CHAIN_SIGNER_HARNESS_REVISION` must equal the reviewed full revision above.
Tests use public synthetic keys and loopback endpoints, send no chain transactions
and stop only their own child. Record source/build/fixture hashes and actual test
results when qualifying a new signer revision.

This host simulation does not qualify a deployed Nitro enclave, custody,
production time/revocation, approved network policies, maximum-load memory/p99,
or the 200 ms cadence. Devnet full-stack and mixed-version/outage validation must
precede Sepolia, then mainnet. Use an independently enrolled devnet policy: the
supplied harness is deliberately restricted to public keys, L2 480 and a synthetic
PBH entry point. Keep proof-result signing within measured execution. No measured
source, measured lockfile or measurement manifest is changed by these adapters.

Devnet acceptance:

1. Enroll separate devnet keys and mTLS identities, pin their public identities,
   and verify protected startup rejects every local-key/override combination.
2. Run PBH traffic through the complete builder and rollup-boost stack. Verify
   nullifier transaction execution, unchanged payload bytes and peer acceptance
   across local/remote publisher versions.
3. Interrupt signing during PBH, payload, start and stop operations; test expiry,
   denied policy and rapid leadership changes. Require bounded failures, no stale
   cache/broadcast, no local fallback and recovery after service restoration.
4. Measure the full stack at its configured cadence under representative maximum
   frames and competing OP signing load. Record p99, deadline failures, memory and
   admission outcomes; host conformance alone does not satisfy this gate.
