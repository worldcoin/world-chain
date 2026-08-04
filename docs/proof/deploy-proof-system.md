# Deploying the Nitro Proof System

This is the operational runbook for deploying World Chain's **Nitro (TEE) proof
system** to a target environment (e.g. `alphanet`, `betanet`) and registering a
running enclave's signing key on-chain.

For _how the system works_ (architecture, attestation, contracts, key lifecycle) see
[`nitro-worker.md`](./nitro-worker.md). For the host/enclave CLI reference see
[`proof-cli.md`](./proof-cli.md). This document is the **deploy + register** procedure
that ties those together, mirroring the `just proof-*` recipes in the repo
[`Justfile`](../../Justfile).

---

## Overview

Deployment is split into numbered phases. Phases 0a–3b are automated by `just`
recipes and can be run together with `just proof-setup <env>`. **Phase 4 (register the
enclave's generated keypair)** is currently a manual step and is documented in full
below.

| Phase | Recipe | What it does |
|------|--------|--------------|
| 0a | `proof-rollup-config-hash` | Compute the rollup config hash |
| 0a | `proof-get-chain-id` | Print the L2 chain ID from the op-node |
| 0b | `proof-get-attestation` | Fetch a **bare** attestation doc from the enclave (for pre-warm) |
| 0b | `proof-get-pcrs` | Print PCR0/PCR1/PCR2 from the EIF on the enclave-launcher |
| 1 | `proof-deploy-nitro` | Deploy the Nitro attestation contract stack |
| 2 | `proof-deploy-system` | Deploy the proof system / dispute game contracts |
| 3a | `proof-certmanager-prewarm` | Pre-warm CertManager with the AWS Nitro CA chain |
| 3b | `proof-approve-pcrs` | Approve the enclave PCR set on the verifier |
| — | `proof-setup` | Runs phases 0a–3b in sequence, wiring addresses automatically |
| **4** | _(manual — see below)_ | **Register the enclave's generated keypair via `registerKey`** |

All recipes take an `env` argument (default `alphanet`) that selects a config file
from [`scripts/proof-envs/<env>.env`](../../scripts/proof-envs). Shell environment
variables always override values from that file. Pass `dry_run=true` to simulate
on-chain steps without broadcasting.

---

## Prerequisites

### Tooling

- [`just`](https://github.com/casey/just), [`foundry`](https://getfoundry.sh)
  (`forge` + `cast`), `kubectl`, `jq`, `nc`, `node`, and a Rust toolchain.
- `git submodule update --init --recursive` so `pkg/contracts/lib/nitro-validator`
  (Base's `nitro-validator` library + its `tools/` scripts) is present.

### A running enclave

The Nitro worker pod (2 containers: `nitro-worker` + `enclave-launcher`) must already
be deployed and `Running` in the target namespace. On `alphanet` this is namespace
`alphanet-world-chain-proof-nitro-worker` in the `crypto-dev-us-east-1` context,
running image `ghcr.io/worldcoin/world-chain-proof-nitro:nightly`. The pod deployment
lives in [`worldcoin/crypto-apps`](https://github.com/worldcoin/crypto-apps), not in
this repo. Phases 0b, 3a, and 4 exec into this pod, so it must be up first.

### Environment config

`scripts/proof-envs/alphanet.env` (committed, non-secret) provides:

```bash
KUBECONTEXT=crypto-dev-us-east-1
PROOF_NAMESPACE=alphanet-world-chain-proof-nitro-worker
PROOF_NITRO_IMAGE=ghcr.io/worldcoin/world-chain-proof-nitro:nightly
OP_NODE_NAMESPACE=alphanet-world-chain-node
OP_NODE_POD=alphanet-world-chain-node-0
OP_NODE_PORT=9545
```

To override any value locally without editing the committed file, create
`scripts/proof-envs/alphanet.local.env` (gitignored).

### Secrets (set in your shell — never committed)

| Variable | Used by |
|----------|---------|
| `PRIVATE_KEY` | deploy-nitro, deploy-system, certmanager-prewarm |
| `OWNER` | deploy-nitro (owner of the verifier + key registry) |
| `OWNER_KEY` | approve-pcrs (owner-only). **Not** required for Phase 4 — `registerKey` is not owner-gated (see Phase 4). |
| `L1_RPC_URL` | all on-chain phases |
| `WORLD_CHAIN_L2_CHAIN_ID` | deploy-system (auto-fetched if unset) |
| `ROLLUP_CONFIG_HASH` | deploy-system (auto-computed by `proof-setup`) |
| `DISPUTE_GAME_FACTORY`, `ANCHOR_STATE_REGISTRY`, `SYSTEM_CONFIG`, `OP_CHAIN_PROXY_ADMIN` | deploy-system (op-deployer proxy addresses) |
| `OP_CHAIN_PROXY_ADMIN_OWNER_PRIVATE_KEY`, `DGF_OWNER_KEY`, `GUARDIAN_KEY` | deploy-system |
| `CERT_MANAGER_ADDRESS`, `NITRO_ATTESTATION_VERIFIER` | prewarm / approve-pcrs (auto-read from deployment JSON if unset) |
| `PCR0`, `PCR1`, `PCR2` | approve-pcrs (auto-fetched from the enclave if unset) |

---

## Quick path: `just proof-setup`

For a full deploy through Phase 3b in one command:

```bash
# Secrets in your shell first (PRIVATE_KEY, OWNER, OWNER_KEY, L1_RPC_URL,
# DISPUTE_GAME_FACTORY, ANCHOR_STATE_REGISTRY, SYSTEM_CONFIG, OP_CHAIN_PROXY_ADMIN,
# OP_CHAIN_PROXY_ADMIN_OWNER_PRIVATE_KEY, DGF_OWNER_KEY, GUARDIAN_KEY).

just proof-setup alphanet
```

`proof-setup` fetches the L2 chain ID and rollup config hash, deploys the Nitro stack,
deploys the proof system, pre-warms CertManager, auto-fetches the enclave PCRs, and
approves the PCR set — wiring contract addresses between steps automatically. Use
`just dry_run=true proof-setup alphanet` to simulate without broadcasting.

After `proof-setup` completes you still need **Phase 4** to register the enclave's
signing key before the worker's proofs will verify on-chain.

---

## Phase-by-phase

### Phase 0a — Rollup config hash & chain ID

```bash
just proof-rollup-config-hash alphanet   # prints the 32-byte rollup config hash
just proof-get-chain-id alphanet          # prints the L2 chain ID (e.g. 480)
```

The hash is sourced (in priority order) from `L2_RPC_URL`, `ROLLUP_CONFIG_URL`,
`ROLLUP_CONFIG`, or by auto port-forwarding to the op-node pod. It must match the
value baked into the deployed contracts and the value the worker computes, or the
proof will be classified as foreign.

### Phase 0b — Inspect the enclave

```bash
just proof-get-attestation alphanet   # bare COSE_Sign1 attestation hex (used by 3a)
just proof-get-pcrs alphanet          # PCR0/PCR1/PCR2 of the running EIF
```

`proof-get-attestation` execs into the `nitro-worker` container and runs
`nitro-worker get-attestation`, which returns a **bare** attestation (no `user_data`,
`nonce`, or `public_key`) used only for CertManager pre-warm. `proof-get-pcrs` runs
`nitro-cli describe-eif` on the `enclave-launcher` container to print the enclave
measurements.

### Phase 1 — Deploy the Nitro attestation stack

```bash
just proof-deploy-nitro alphanet
```

Runs `scripts/devnet/DeployNitro.s.sol`, deploying
`P384Verifier → CertManager → NitroAttestationVerifier → NitroEnclaveKeyRegistry →
NitroProofVerifier`. Addresses are written to
`pkg/contracts/deployments/alphanet-nitro.json`. The verifier is deployed with an
**empty PCR allowlist** — Phase 3b fixes that.

### Phase 2 — Deploy the proof system contracts

```bash
just proof-deploy-system alphanet
```

Runs `scripts/devnet/DeployProofSystem.s.sol`, deploying the `MultiProofGame`
implementation and wiring it into the OP Stack `DisputeGameFactory` /
`AnchorStateRegistry`. Requires the op-deployer proxy addresses and owner keys listed
above. Tunable via `PROOF_SYSTEM_BLOCK_INTERVAL` (default 10), `PROOF_THRESHOLD`
(default 2), `DELAYED_WETH_DELAY` (default 300), and `SET_RESPECTED_GAME_TYPE`
(default true). Addresses are written to
`pkg/contracts/deployments/alphanet-proof-system.json`.

### Phase 3a — Pre-warm CertManager

```bash
just proof-certmanager-prewarm alphanet
```

Fetches a bare attestation (Phase 0b), runs
`pkg/contracts/lib/nitro-validator/tools/hinted_attestation_calls.js prepare` to
produce the cold-cert call plan with pre-computed P-384 hints, simplifies it to
parallel arrays with `jq`, and submits each uncached cert via
`scripts/devnet/PrewarmCertManager.s.sol`. This caches the AWS Nitro CA certificate
chain on-chain (~1.5M gas per cert, `--slow`), which is a **prerequisite for any
`registerKey` call** — without it the first registration would exceed the block gas
limit.

`CERT_MANAGER_ADDRESS` is auto-read from `alphanet-nitro.json` if not set in the
shell.

### Phase 3b — Approve the PCR set

```bash
just proof-approve-pcrs alphanet
```

Calls `NitroAttestationVerifier.approvePCRSet(keccak256(pcr0), keccak256(pcr1),
keccak256(pcr2))` as the contract owner (`OWNER_KEY`). PCR values are auto-fetched from
the running enclave if `PCR0/1/2` are unset. Until a PCR set is approved,
`verifyAttestation` reverts with `PCRSetNotApproved` and no key can register.

> **Dev/placeholder note:** an enclave started in debug mode (or the worker running in
> placeholder mode) reports **all-zero PCRs**. In that case the approved set is the
> zero triple — this is dev/test only and provides no attestation guarantees. See the
> "Dev/Test Mode" section of [`nitro-worker.md`](./nitro-worker.md).

---

## Phase 4 — Register the enclave's generated keypair

This is the actual **"register the nitro worker"** step. The keypair being registered
is the enclave's **ephemeral secp256k1 signing key**, which the enclave generates from
NSM hardware entropy on every boot (see `init_signing_key` in
`proofs/nitro/src/enclave.rs`). Registration binds that generated public key to the
approved PCR set on-chain so that `NitroProofVerifier.verify` will accept `ecrecover`
signatures produced by this enclave.

> **Access control — `registerKey` is NOT owner-gated.**
> `NitroEnclaveKeyRegistry.registerKey(bytes,bytes,bytes)` is a plain `external`
> function with **no `onlyOwner` modifier** — anyone can call it, and it only needs a
> funded L1 key to pay for gas (it does **not** require `OWNER`/`OWNER_KEY`).
> Authorization is **purely cryptographic**: the call reverts unless
> `NitroAttestationVerifier.verifyAttestation` succeeds, which requires a genuine
> AWS-signed COSE_Sign1 attestation, a valid P-384 certificate chain up to the
> hardcoded AWS Nitro Root CA, **and** a PCR triple that is already in the
> owner-approved allowlist (`approvePCRSet`, Phase 3b). In other words the owner gates
> _which enclave images_ may register (at the PCR/image level via `approvePCRSet`),
> not each individual `registerKey` transaction. Only `revokeKey` (registry) and
> `approvePCRSet`/`revokePCRSet` (verifier) are `onlyOwner`.

> **Tooling gap (as of this writing):** there is no `just` recipe or CLI subcommand
> that performs Phase 4 end-to-end. The bare `nitro-worker get-attestation` /
> `just proof-get-attestation` used in Phase 3a returns an attestation **without** the
> `public_key` field, so it **cannot** be used to register a key. Registration needs a
> `public_key`-embedding attestation produced by the enclave's `EnclaveRequest::PublicKey`
> handler (`handle_public_key` in `proofs/nitro/src/enclave.rs`). The steps below use
> the primitives that exist today; opening this up as a `just proof-register-key`
> recipe is tracked as follow-up work.

### Prerequisites for Phase 4

- Phases 1–3b complete (contracts deployed, CertManager pre-warmed, PCR set approved).
- The enclave is running and reachable over vsock from the `nitro-worker` container.
- `NITRO_ENCLAVE_KEY_REGISTRY` — the `nitroEnclaveKeyRegistry` address from
  `pkg/contracts/deployments/alphanet-nitro.json`.

```bash
REG=$(jq -r '.nitroEnclaveKeyRegistry' pkg/contracts/deployments/alphanet-nitro.json)
```

### Step 4.1 — Obtain a public-key-embedding attestation

Send the enclave a `PublicKey` request so it returns an attestation document whose
`public_key` field holds the enclave's generated secp256k1 key (33-byte compressed
SEC1) plus the uncompressed key for convenience. On the host that shares the vsock
namespace with the enclave (i.e. inside the `nitro-worker` container), use the
`world-chain-prover-nitro` library path / `NitroProver::get_public_key_async`.

> The bare `get-attestation` subcommand is **not** sufficient here — it omits
> `public_key`. If a `PublicKey` CLI/recipe is not yet available in your build, fetch
> the attestation via the `EnclaveRequest::PublicKey` round-trip
> (`NitroProver::get_public_key_async`) and save the raw COSE_Sign1 bytes to
> `attestation.bin`.

### Step 4.2 — Split the attestation into TBS + signature

```bash
NITRO_VALIDATOR=<NitroValidator address>   # from the nitro-validator lib deployment
ATTESTATION_HEX=$(xxd -p -c0 attestation.bin)
TBS_AND_SIG=$(cast call "$NITRO_VALIDATOR" "decodeAttestationTbs(bytes)" 0x$ATTESTATION_HEX)
# → decode into $TBS (COSE_Sign1 TBS bytes) and $SIG (96-byte r||s P-384 signature)
```

### Step 4.3 — Compute the P-384 attestation hints

Registration uses hinted on-chain P-384 verification (Base `nitro-validator` PR #28).
Extract the leaf certificate public key from the attestation's cabundle
(`$LEAF_PUBKEY_HEX`), then:

```bash
cargo build -p world-chain-proof-nitro --bin p384-hints
ATTEST_HINTS=$(cargo run -p world-chain-proof-nitro --bin p384-hints -- attestation \
  --attestation @attestation.bin \
  --leaf-pubkey  "$LEAF_PUBKEY_HEX")
```

### Step 4.4 — Call `registerKey`

```bash
# Any funded L1 key works here — registerKey is not owner-gated (see note above).
cast send "$REG" \
  "registerKey(bytes,bytes,bytes)" \
  "$TBS" "$SIG" "$ATTEST_HINTS" \
  --rpc-url "$L1_RPC_URL" --private-key "$PRIVATE_KEY"
```

`registerKey` re-verifies the attestation on-chain (COSE_Sign1, P-384 signature, cert
chain via CertManager, PCR triple against the approved allowlist), extracts the
enclave's uncompressed public key, and stores `keccak256(publicKey)` as `Active`. The
key lifecycle is strictly `Unknown → Active → Revoked` (see
[`nitro-worker.md`](./nitro-worker.md#key-lifecycle)).

### Step 4.5 — Verify registration

```bash
# The 65-byte uncompressed public key (0x04 || X || Y) returned by the enclave.
cast call "$REG" "isKeyRegistered(bytes)(bool)" "$ENCLAVE_PUBKEY" --rpc-url "$L1_RPC_URL"
# → true
```

Once `isKeyRegistered` returns `true`, the worker's proofs will be accepted by
`NitroProofVerifier` and you can exercise the end-to-end path with the `prover-cli`
tool (see [`proof-cli.md`](./proof-cli.md)):

```bash
cargo run -p prover-cli -- \
  --l2-rpc-url          "$L2_RPC_URL" \
  --l1-rpc-url          "$L1_RPC_URL" \
  --prover-service-url  "$PROVER_SERVICE_URL" \
  --poll
```

---

## Automated self-registration

Because `registerKey` is not owner-gated and is authorized purely by attestation + the
owner-approved PCR allowlist (Phase 4), the worker can register **itself** on boot — no
human/owner signature needed. The owner still controls _which enclave images_ may register
via `approvePCRSet`. This is implemented (world-chain PR #938); Phase 4's manual steps
remain as a fallback.

One shared flow (`register_enclave_key` in `proofs/nitro/src/register.rs`) fetches the
`public_key`-embedding attestation over vsock, builds the `registerKey` calldata (TBS +
P-384 hints), submits it, and confirms `isSignerRegistered`. It is idempotent (treats
already-registered / concurrent-registration races as success) and retries transient RPC /
nonce failures while failing fast on deterministic reverts. Entry points:

- **`world-chain-prover-nitro register`** — one-shot CLI (dev/local).
- **`nitro-worker register`** — same, on the worker binary (used in-pod).
- **`nitro-worker run --auto-register`** — registers at startup before leasing jobs.
- **`just proof-register-key <env>`** — wraps the in-pod invocation.

Config: `NITRO_ENCLAVE_KEY_REGISTRY` (registry address), `L1_RPC_URL` (reused for the tx),
and a funding key via `REGISTER_PRIVATE_KEY` (falls back to `PRIVATE_KEY`).

## Kubernetes deployment (alphanet auto-register)

The alphanet worker (`worldcoin/crypto-apps`
`values/devnets/alphanet/world-chain-proof-nitro-worker`) self-registers on boot. The
pieces that make that work — and that the deployment values only reference tersely:

**Enclave in production mode.** The enclave-launcher sidecar runs `nitro-cli run-enclave`
**without** `--debug-mode` (`ENCLAVE_DEBUG_MODE=false`). Debug mode makes the NSM report
all-zero PCRs; production mode reports the EIF's real PCR0/1/2, which must match the set
approved on-chain (Phase 3b / `alphanet-nitro.json`) for `registerKey` to succeed. No EIF
rebuild is needed — it's a runtime flag.

**Keep-alive via probes, not `nitro-cli console`.** `nitro-cli console` only attaches to
debug-mode enclaves, so the launcher instead starts the enclave, extracts the ID/CID with
`jq` (`nitro-cli describe-enclaves | jq -r '.[0].EnclaveID'` / `.EnclaveCID`), writes the
CID to the shared `/run/nitro-shared/enclave-cid` volume for the worker, `trap`s
`nitro-cli terminate-enclave` on shutdown, touches `/tmp/enclave-initialized`, then blocks
on `sleep infinity`. Kubernetes `startup`/`liveness`/`readiness` probes assert the enclave
is `RUNNING` (`nitro-cli describe-enclaves | jq -e '.[].State == "RUNNING"'`; startup /
readiness also check the marker) and restart the pod on failure. This mirrors the
world-chat secure-enclave deployment pattern.

**Funding-key provisioning chain.** `REGISTER_PRIVATE_KEY` is not stored in git. It flows:
`worldcoin/infrastructure` Terraform (`crypto/dev/us-east-1/alphanet.tf` — `random_bytes`
→ AWS Secrets Manager `proof_nitro_worker_register_key.hex`) → the `kube-ops` controller
syncs it into the namespace's `application` Kubernetes Secret → the `common-app` chart's
`mountSecrets` mounts it at `/etc/secrets` → the worker's startup shell runs
`export REGISTER_PRIVATE_KEY=$(cat /etc/secrets/proof_nitro_worker_register_key.hex)`.
Fund the derived address with (Sepolia) ETH after apply. Mirrors the defender/challenger
key pattern.

**PCR verification.** The approved PCR0/1/2 in `alphanet-nitro.json` are captured from the
EIF via `nitro-cli describe-eif` (build-time, independent of debug/production runtime), so
production mode reports exactly those — **provided the deployed EIF image is the one they
were captured from**. Confirm before rollout, and re-run `just proof-approve-pcrs` if the
EIF was rebuilt. Host-side PCR pinning on the worker (`PCR0/1/2` env) is optional (the
on-chain registry already enforces the set); leaving it unset avoids a host-side hard-fail
if the EIF drifts.

---

## Re-registration & upgrades

- **Enclave restart:** the signing key is ephemeral, so a restarted enclave generates
  a **new** keypair that must be re-registered (repeat Phase 4). The old key stays
  `Active` in the registry but can no longer sign anything.
- **New enclave image (new PCRs):** approve the new PCR triple with
  `just proof-approve-pcrs` (both old and new sets are approved during overlap), roll
  out the new enclaves, register each new key (Phase 4), then call
  `revokePCRSet(oldPcr0, oldPcr1, oldPcr2)` once migration is complete.
- **Revocation:** `NitroEnclaveKeyRegistry.revokeKey(publicKey)` (owner-only) is
  permanent — a revoked key hash can never be re-registered.

---

## Troubleshooting

| Symptom | Likely cause |
|---------|--------------|
| `registerKey` reverts / OOG on first call | CertManager not pre-warmed (Phase 3a) |
| `registerKey` reverts with `PCRSetNotApproved` | PCR set not approved (Phase 3b), or enclave PCRs differ from the approved set |
| `isKeyRegistered` is `false` after `registerKey` | Registered the bare attestation (no `public_key`) instead of the `PublicKey`-request attestation |
| Proof classified as foreign | `ROLLUP_CONFIG_HASH` mismatch between contracts and worker |
| `proof-get-attestation` errors | Nitro worker pod not `Running`, or enclave not started yet |
