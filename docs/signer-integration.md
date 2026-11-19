# Protected World Chain signing

[BLO-1335](https://linear.app/worldcoin/issue/BLO-1335) tracks these adapters;
[BLO-1446](https://linear.app/worldcoin/issue/BLO-1446) records the v0.1.0 pairing baseline;
[BLO-1453](https://linear.app/worldcoin/issue/BLO-1453) tracks the extended profile.
Pin host pairing to [KMS TEE Signer v0.1.1](https://github.com/worldcoin/kms-tee-signer/releases/tag/v0.1.1),
source `dbd2ca689b832f10bbe0666c9beccc576404d0dc`.
The [V1 contract](https://github.com/worldcoin/kms-tee-signer/blob/dbd2ca689b832f10bbe0666c9beccc576404d0dc/docs/signer-protocol-v1.md)
and [client-pairing runbook](https://github.com/worldcoin/kms-tee-signer/blob/dbd2ca689b832f10bbe0666c9beccc576404d0dc/docs/runbooks/host-client-pairing.md)
are the compatibility baseline. The published Linux/Nitro runtime uses the same
source; its new EIF SHA256 is
`ac1d0349493141985ad6acdfbaa1210b03835343afa3e7a30e56a14865bf7d85`.
Verify distribution checksums and the SSH signature using an independently trusted
signing-key fingerprint, following the release's `RELEASE-GUIDE.md` asset and
`release-manifest.json`. Prior artifact measurements do not qualify this EIF.

The v0.1.1 development profile accepts 261,909 complete batcher calldata bytes
and at most 16,777,216 L1 transaction gas. World Chain's PBH and Flashblock wire
contracts and limits are unchanged. This profile does not change L2 block gas;
OP batcher pairing and shared-load qualification are tracked separately.

## Roles and wire compatibility

| Role | Outbound signer method | Required result validation |
| --- | --- | --- |
| Grant authorizer | `teekms_signFlashblockAuthorizationV1` | Ed25519 over BLAKE3 of payload ID, little-endian timestamp and publisher key; verify the independently pinned authorizer key |
| Publisher | `teekms_signFlashblockPublicationV1` | Ed25519 over BLAKE3 of the exact message RLP followed by grant RLP; verify the independently pinned publisher key |
| PBH wallet | `eth_signTransaction` | Canonical signed type-2 bytes, every original unsigned field, low-S signature and independently configured recovered address |

These are outbound operations, not new public World Chain RPC methods.
The node normally receives authorizer grants from rollup-boost. A sequencer can
instead request grants through `--flashblocks.authorizer-signer-config` using
its separately enrolled authorizer credential. Payload/start/stop encodings and existing signature preimages
are unchanged. Existing Ed25519 preimages contain no chain ID: enroll separate
network keys, identities and policies. Peer identity keys and proof-result keys
remain separate; see [proof signing boundaries](proof/signing-boundaries.md).

## Configuration

Local signing retains the existing flags. An enabled builder requires an explicit
local key or remote configuration. Nodes without either hold no PBH signer and
fail explicitly if asked to sign a nullifier transaction. There is no embedded
default PBH private key or fallback after a remote failure.

Either remote signer option enables protected mode. For PBH and publication,
supply both signer configurations:

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
PBH signing when `--builder.enabled` is set. Receive-only nodes need no publisher
signing credentials.

For publication on a network with PBH disabled, omit the PBH builder and its
credentials and set zero reserved capacity explicitly:

```sh
world-chain node --flashblocks.enabled \
  --flashblocks.signer-config /run/secrets/publisher-signer.json \
  --flashblocks.authorizer-vk "$REVIEWED_AUTHORIZER_PUBLIC_KEY" \
  --pbh.verified-blockspace-capacity 0
```

This mode creates no PBH wallet. To obtain in-process grants, additionally supply
`--flashblocks.authorizer-signer-config /run/secrets/grant-signer.json`. It uses
the same JSON schema as the publisher below, with its own authorizer credential
and key ID. Its expected public key must match the explicit
`--flashblocks.authorizer-vk` pin. Without that option, grants still come from
the external producer. Zero capacity does not permit local keys or force-publish.

The signer policy/lease issuer must authorize the elected sequencer and enrolled
publisher for that network; Engine JWT possession and health checks do not prove
leadership. The job reserves its publishing generation before grant I/O. Resolve,
drop, deadline, a newer local intent or an accepted peer handover cancels that
generation, so a late grant cannot reactivate it. Grant failures disable optional
publication for the job while execution continues, with no local/external grant
fallback. A pending grant does not delay execution or advance the Flashblock
index. Derived `noTxPool` jobs do not request grants. Network-specific policy,
leadership transfer, actual peers, Nitro and recovery still require qualification.

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

For a remote host or Nitro relay, follow the release's
[remote connection runbook](https://github.com/worldcoin/kms-tee-signer/blob/dbd2ca689b832f10bbe0666c9beccc576404d0dc/docs/runbooks/remote-signing.md).
The endpoint hostname is also the verified TLS name; use the documented loopback
tunnel or a matching certificate, preserving end-to-end mTLS.

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
Set `signer_target` to an absolute, disposable build directory. From a clean signer
checkout at the full source revision above, build its release profile with the
published toolchain and both role features:

```sh
rustup run 1.99.1 cargo build --release --locked -p tee-kms-signer \
  --features flashblocks,transactions --example block-runtime-simulation \
  --target-dir "$signer_target"
export WORLD_CHAIN_SIGNER_HARNESS_BINARY="$signer_target/release/examples/block-runtime-simulation"
export WORLD_CHAIN_SIGNER_HARNESS_REVISION=dbd2ca689b832f10bbe0666c9beccc576404d0dc
```

From the World Chain checkout, run the actual-client suite:

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

The numbered release supplies a development/test runtime; it does not authorize
production activation. This host simulation does not qualify a deployed Nitro
enclave, custody, production time/revocation, approved network policies, maximum-load memory/p99,
or the configured publishing cadence. Qualify representative 5/s and 6/s traffic
with per-role and aggregate accounting under [BLO-1447](https://linear.app/worldcoin/issue/BLO-1447);
the release's 200/s stress results do not establish that acceptance or the 2 ms
production p99 target. Devnet full-stack and mixed-version/outage validation must
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
