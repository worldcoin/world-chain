# Incident: World Chain builder peering & fleet NotReady — crypto-dev-eu-central-2 (2026-07-01)

Status: root cause identified · readiness-probe patch prepared (crypto-apps branch) · relay fix requires Alchemy

---

## 1. Executive summary

Three distinct problems compounded into a fleet-wide outage:

1. **Chronic (the disease): the builders have no working external EL P2P peers.**
   Every eth session a builder opens to the four Alchemy relays is killed by the relay
   **immediately after the eth Status exchange** — reth-based relays send
   `Disconnect(ProtocolBreach)`, the op-geth relay sends `Disconnect(SubprotocolSpecific)`.
   Deep-dive in §3: this is **not** a wire-format or ForkID incompatibility; the
   evidence points to **relay-side (Alchemy) peer access-control/config drift** —
   confirmation requires their logs/config. It has been broken long enough for the
   snapshotter (which has no inbound peers at all) to fall **~636k blocks (~15 days)** behind.

2. **Trigger: three crypto-apps rollouts in one afternoon** (#592 17:24Z → #593 revert
   17:53Z → #594 re-apply 19:15Z). #594 replaced **all three builder pods within 40s**,
   because the readiness probe (`eth_syncing == false`) **passes transiently at boot**
   before the engine notices it is behind — the StatefulSet rolling gate never held.
   (The manual `kubectl` image revert was also silently undone: the StatefulSet is
   ArgoCD-managed; all changes must go through crypto-apps.)

3. **Deadlock: restart ⇒ unsafe-gap ⇒ nothing can fill it.** Each restart loses the
   in-memory unsafe chain. op-node CL gossip keeps delivering new tips (CL peering is
   healthy, 5–8 peers), but their parents are missing; op-node's CL range-sync loop
   (`requesting engine missing unsafe L2 block range`) never succeeds in this
   deployment; EL P2P has no usable peers (relays reject us; the other builders,
   restarted simultaneously, are missing the *same* range, so reth's downloader
   penalizes and drops them — the observed mutual connect→drop livelock,
   `remaining_peers=0`). The only healing path is **L1 derivation** (~30–40 min),
   during which `eth_syncing != false` keeps readiness failing fleet-wide. This
   self-healed twice during the day (catch-up bursts of 1,268 / 1,338 blocks) before
   the next rollout reset it.

**Ruled out with evidence** (each disproven, §5): the alloy-eip7928/BAL RLP change,
`--flashblocks.access-list`, world-chain PR #783, JWT secrets, clock skew, ForkID
mismatch, eth/69 format drift between reth v2.2.0/v2.3.0, node crashes.

---

## 2. Timeline (UTC, 2026-07-01)

| Time | Event |
|---|---|
| ~Jun 16 → | snapshotter stuck at 0 EL peers, falls ~636k blocks behind (chronic EL peering breakage window) |
| 13:21–16:30 | Builders healthy (300 commits/10min) with chronic ~40 relay-session kills/10min |
| 16:38 | builder-2 pod replaced → 30 min NotReady → derivation catch-up at 17:1x (1,268 blocks) |
| 17:24:57 | crypto-apps **#592** (image v2.3.0 → sha-9828474) → builder-2 replaced 17:25:41 |
| 17:53:56 | crypto-apps **#593** (revert) → builder-2 replaced 17:54 / 17:55 |
| 18:0x | Derivation catch-up (1,338 blocks); fleet healthy by 18:51 |
| 19:15:18 | crypto-apps **#594** (re-apply) → **all 3 pods replaced 19:15:46–19:16:26** → fleet NotReady |
| 19:4x+ | Heads pinned near 33,348,3xx; safe head advancing via L1 derivation; EL peers 0 on all three |

---

## 3. ProtocolBreach deep-dive (the peering root cause)

### Observed
- Builder logs: `Session established … client_version=reth/v2.2.0-7680d6d` followed
  **~111µs later** by `disconnected: breach of protocol` — for every outbound session
  to relays `0xa2feb11a`, `0x9d9a5df8`, `0xe1ae5f2e`. The op-geth relay `0xa0b22939`
  kills sessions with `some other reason specific to a subprotocol` (geth code 0x10).
- All relay EL sessions are **outbound only** (0 inbound in the sampled window).
- `p2p rlpx ping` from a builder pod succeeds against the relays (full `HelloMessage`
  exchanged): relay-2 = `reth/v2.2.0` (eth/66–69), relay-3 = `Geth/v1.101609.0`
  (eth/68–69, snap/1). So TCP + RLPx + Hello are all fine from pod source IPs.
- The same probe from a laptop **times out at TCP** → the relays IP-allowlist inbound
  connections. Separately, the pods' outbound dials to relay **op-node port 9222 time
  out at TCP** (`i/o timeout`) while 30303 connects — per-port security-group drift.

### Analysis (why it is NOT the usual suspects)
- **Not eth/69 wire-format drift**: `crates/net/eth-wire-types/src/status.rs` has the
  *identical git blob* (`244bcd35…`) at reth v2.2.0 and v2.3.0; `StatusEth69` layout is
  byte-identical. The relay completes the Status decode (its rejection follows its own
  well-formed Status by µs — sent back-to-back after validating ours).
- **Not genesis / networkid / protocol-version**: those checks are symmetric — the
  builders *accept* the relay's Status (we log `Session established`), so those fields
  match; the relay comparing the same values cannot fail.
- **Not ForkID**: the only direction-dependent stock check is EIP-2124 validation, but
  the builders announce `ForkId { hash: 0x07b62f25, next: 0 }` (confirmed live via
  `eth_config`/EIP-7910). With `next = 0` and the relay serving the same chain at head,
  every strict-EIP-2124 rejection path (stale-remote, passed-announced-fork,
  unknown-hash-with-local-future) requires a contradiction with the builders' accept of
  the relay's Status. There is **no assignment of relay-side ForkID values consistent
  with both sides' observed behavior under stock validation code.**
- The mystery `decode error in eth handshake` frames are unrelated noise: decoded, they
  are eth/69 Statuses with `networkid=137` and `latest≈89.47M` — Polygon peers found
  via public discovery (our chain id is 69420).

### Conclusion
Stock client code cannot produce "relay rejects our Status while we accept theirs, with
equal symmetric fields and `next=0`". Therefore the rejection is a **non-stock,
relay-side peer admission policy** (node-ID and/or source-IP allowlist enforced at the
eth handshake layer by Alchemy's relay stack), consistent with:
- IP-level allowlisting demonstrably present (laptop TCP-dropped; pods allowed on
  30303 but **dropped on 9222**),
- the breakage window (~Jun 16) matching no builder-side wire change,
- uniform behavior across two different relay client families (reth + op-geth), each
  rejecting with its own family-specific handshake-rejection disconnect code.

**Confirmation + fix owner: Alchemy.** Ask them to check relay-side logs for the
rejection reason for peers from egress IP **16.62.171.158** (crypto-dev NAT) /
the builder NLB IPs, and to align:
1. allowlists for the crypto-dev builder node IDs
   (`6c90bb18…`, `3bc61595…`, `95ebe846…`) and egress IPs,
2. the security groups on **tcp/9222** (op-node CL) — currently dropping pod traffic
   (`dial tcp4 …:9222: i/o timeout` on both builders), and
3. relay client versions (reth v2.2.0 is two majors behind the builders).

### Patches (what we control)
| # | Change | Where | Effect |
|---|---|---|---|
| P1 | **Readiness probe: head-freshness instead of `eth_syncing`** (§4) | crypto-apps | StatefulSet rolling updates can no longer leapfrog a still-behind pod; fleet-wide simultaneous restarts eliminated |
| P2 | **Enable op-node CL alt-sync serving**: `--p2p.sync.req-resp=true` on builders + snapshotters | crypto-apps | restart gaps heal in seconds from healthy CL peers instead of ~35 min via L1 derivation; removes dependence on relay EL peering |
| P3 | Exempt trusted peers from downloader-failure session drops (retry/backoff instead of drop) | world-chain (`crates/p2p` / network config) | prevents the builder↔builder mutual-drop livelock when all are missing the same range |
| P4 | EL peer-count / disconnect-reason / unsafe-gap metrics + alerts (p2p WARN/TRACE never reaches Datadog) | world-chain + Datadog | this incident becomes one dashboard glance |
| P5 | Report reth CLI bug: `reth p2p header --port` is ignored (listener hardcodes chain default; `crates/cli/commands/src/p2p/mod.rs` builds the network without applying `rlpx_socket` to the listener) | upstream reth | unblocks future on-pod handshake probes |

---

## 4. Readiness probe patch (P1) — prepared in crypto-apps

**Problem**: `[[ $(cast rpc eth_syncing) == "false" ]]` is true for a freshly booted
node for the first seconds (the engine hasn't seen a forkchoice beyond its head yet),
so a rolling update sees Ready and immediately replaces the next pod — on 2026-07-01
all three builders restarted within 40 s. It then stays false for the entire
(unavoidable) catch-up, which is correct — the problem is only the boot-time hole.

**Fix**: gate readiness on **chain-head freshness** — unsatisfiable until the node has
genuinely caught up to the live head (2 s blocks; 45 s tolerance):

```bash
set -Eeuo pipefail
JWT=$(cat /etc/secrets/rollup_boost_jwt.hex)
# Ready iff the chain head is fresh (timestamp within MAX_AGE_SECONDS of now).
# A bare `eth_syncing == false` check passes transiently right after a restart,
# before the engine notices it is behind the unsafe head; that let StatefulSet
# rolling updates replace every replica at once (2026-07-01 incident).
MAX_AGE_SECONDS=45
head_ts=$(cast block latest --field timestamp \
  --rpc-url http://localhost:8545 \
  --jwt-secret "$JWT" 2>/dev/null) || exit 1
now=$(date +%s)
(( now - head_ts <= MAX_AGE_SECONDS ))
```

Applied to `values/world-chain/world-chain-builder/values-common.yaml` and the chart
default `charts/worldchain-node/values.yaml` on branch
`fix/builder-readiness-head-freshness` (see PR).

Additional rollout hardening (same PR if chart supports): `minReadySeconds: 120` on the
StatefulSet so a pod must hold Ready for 2 minutes before the controller proceeds.

---

## 5. Ruled out (with the disproving evidence)

| Hypothesis | Disproof |
|---|---|
| alloy-eip7928 0.3.7→0.4.3 RLP change broke flashblocks P2P | `--flashblocks.access-list` not set (default false) → `access_list_data=None`; field is `#[rlp(trailing)]` `Option` → **zero bytes on wire**; `crates/p2p` diff v2.3.0→HEAD empty; zero decode/verify warnings in logs |
| The sha-9828474 deployment itself | Symptoms reproduce identically on reverted v2.3.0; both lockfiles pin the same reth commit (`9384bc53`) and identical alloy stack |
| world-chain PR #783 (`race_pending_payload`) | Not present in v2.3.0, where the issue reproduces |
| JWT secrets | `rollup_boost_jwt.hex` sha256-identical on all 3 pods; `Invalid JWT` log spam is an unauthenticated scraper on authrpc (0.0.0.0), benign |
| Clock skew | All 3 nodes within ~50 ms of reference; flashblocks timestamp-window warnings entirely absent from logs |
| ForkID / hardfork mismatch between builders | `genesis.json`/`rollup.json` md5-identical across pods; live fork schedules identical (`admin_nodeInfo`) |
| eth/69 Status format drift reth 2.2↔2.3 | Identical `status.rs` blob SHA at both tags |
| Node crashes/OOM | All four reth shutdowns in the incident window were graceful SIGTERMs; `restartCount=0` (pods *replaced*, not crashed) |
| eth-handshake "decode error" frames from relays | Frames decode to Polygon (networkid 137, head ~89.47M); our chain is 69420 |

---

## 6. Key diagnostics reference (for next time)

- Builder p2p WARN/TRACE only exists on-pod: `/data/logs/69420/reth.log*` (Datadog
  ships INFO+ only). reth container name: `reth`.
- Real chain config = `/data/alchemy-sepolia-genesis.json` (jq-pinned at pod start);
  `/data/genesis/genesis.json` is a stale leftover — do not trust it.
- op-node RPC on `:9545`: `opp2p_peers`, `optimism_syncStatus`.
- `cast rpc --jwt-secret "$JWT"` (not `--jwt`) against `:8545`.
- Builders' internet egress IP (for allowlists): `16.62.171.158`.
- Useful live checks: `admin_peers | jq length`, `eth_config` (EIP-7910 forkId),
  `world-chain p2p rlpx ping enode://…` (works on-pod; needs IP literal, not DNS).
