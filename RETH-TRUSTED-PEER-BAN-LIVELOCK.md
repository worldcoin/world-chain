# Trusted-peer ban livelock: builders permanently dropping each other's sessions

Verdict: **yes — two reth bugs, triggered by a world-chain bug.** All three verified by
code reading (reth v2.3.0 `9384bc53` and current main), live cluster evidence
(crypto-dev + crypto-stage), and a **local two-node reproduction**.

Fix branch: `~/work/reth` → **`fix/trusted-peer-ban-livelock`** (compiles; tests updated/added).

---

## 1. Symptom

In a mesh of N nodes that all list each other in `--trusted-peers` (the World Chain
builders), sibling sessions establish and are killed within ~100µs by the remote with
wire `Disconnect(0x00)` (`DisconnectRequested`), on a synchronized ~45s retry cycle,
indefinitely — even when every node is fully healthy and synced:

```
# dev builder-1 (dialer), healthy fleet, eth_syncing=false:
22:12:11.766475 DEBUG net: Session established remote_addr=172.20.66.34:30303 … peer_id=0x6c90bb18… kind=outgoing
22:12:11.766604 DEBUG net::session: failed to receive message err=disconnected: disconnect requested
22:12:53.619   … identical …
22:13:49.947   … identical …            ← every ~45s, forever

# dev builder-0 (receiver) — same TCP connection:
22:12:11.766366 DEBUG net: Session established remote_addr=10.84.181.240:51634 … peer_id=0x3bc61595… (incoming)
```

Result on crypto-dev-eu-central-2: EL `admin_peers` shows **zero builder↔builder
sessions on all three builders**. Same on crypto-stage (World Chain Sepolia): none of
the sibling peer IDs appear in any builder's `admin_peers` (masked there by ~20 public
peers; dev has no fallback peers, so restarts deadlock the fleet — 2026-07-01 incident).

Builder-0 metrics after ~14 days: `reth_network_outbound_disconnect_requested 235`,
`reth_network_backed_off_peers_connection_error 32501`.

## 2. Causal chain

```
world-chain flashblocks state corruption on reconnect (§3)
        │  silent ReputationChangeKind::BadMessage on the trusted sibling,
        │  every session (re)establish  (−2048/hit, trusted cap)
        ▼
reth: trusted peers are bannable via accumulated reputation      [reth bug A]
        │  ~25 hits → BANNED_REPUTATION → ban_peer()
        │  (also reachable directly: on_connection_failure() bans trusted peers
        │   unconditionally on any is_fatal_protocol_error())
        ▼
reth: banned trusted peer's *incoming* sessions killed at establish   [reth bug B]
        │  on_incoming_session_established → ban_list.is_banned_peer →
        │  PeerAction::DisconnectBannedIncoming → StateAction::Disconnect{reason: None}
        │  → active.rs: reason.unwrap_or(DisconnectRequested)  ⇒ opaque 0x00 on the wire
        ▼
dialer: received 0x00 ⇒ BackoffKind::Low ⇒ ~45s retry ⇒ re-establish ⇒
new BadMessage penalties keep reputation pinned below the ban threshold ⇒ LIVELOCK
```

Why it never resolves: both sides run the same 45s backoff constant, the flashblocks
penalty re-fires on *every* reconnect, and reth's reputation tick recovery is slower
than the penalty rate. Why dev only: dev's builders were restarted simultaneously
(phase-locking the retry timers, maximizing reconnect churn) and dev has no other
same-chain peers to mask the broken mesh.

## 3. The trigger: world-chain flashblocks peer-state corruption (companion bug)

`crates/p2p` keys per-peer state by `peer_id`
(`FlashblocksP2PState.peers: HashMap<PeerId, FlashblocksPeerState>`), but reth's
simultaneous-dial resolution routinely creates **two short-lived overlapping
connections to the same peer** (both sides dial each other; the duplicate is killed
with `AlreadyConnected`). Sequence:

1. conn1 (incoming) active; flashblocks entry for peer P exists.
2. conn2 (outgoing) establishes → `FlashblocksConnection::new` → `on_peer_connected`
   **replaces** P's entry (fresh state, new outbound_tx).
3. conn1 is killed as duplicate → its `Drop` → `on_peer_disconnected` **removes P's
   entry — the one now belonging to conn2**.
4. Control messages on conn2 (`RequestFlashblocks`/`Accept`, driven every cycle by
   `--flashblocks.force_receive_peers`) hit missing/stale state → the error paths in
   `connection.rs` call `reputation_change(P, BadMessage)` **with no log line**.

Local repro trace (see §4) — the penalty lands ~250µs after every establish:

```
23:06:53.500193 DEBUG net: Session established … peer_id=0x466d7fca… (sibling)
23:06:53.500524 TRACE net::peers: applying reputation change … kind=BadMessage
23:06:53.500536 TRACE net::peers: applied reputation change reputation=-4096 banned=false kind=BadMessage
23:06:53.503612 TRACE net::session: already connected session_id=SessionId(3) …
```

This explains the **zero warnings** in dev logs despite constant penalties, and the
chronic `flashblocks peer disconnected … remaining_peers=0` churn.

## 4. Local reproduction (2 nodes, ~5 minutes)

```bash
WC=world-chain  # v2.3.0 (reth 9384bc53); plain `reth node` + a penalizing subprotocol behaves the same
# secrets: a/sk.hex = 0x11…, b/sk.hex = 0x22…; AUTH = 0x33…
$WC node --chain genesis.json --disable-discovery --datadir a --p2p-secret-key a/sk.hex \
  --port 30311 --ipcpath /tmp/reth-a.ipc --authrpc.port 8651 \
  --trusted-peers "enode://<B_ID>@127.0.0.1:30312" \
  --flashblocks.enabled --flashblocks.builder_sk $(cat a/sk.hex) \
  --flashblocks.override_authorizer_sk $AUTH --flashblocks.force_publish \
  --flashblocks.force_receive_peers <B_ID> \
  --log.stdout.filter "net::peers=trace,net::session=trace,net=debug,flashblocks=trace"
# node B: mirror image (30312 ↔ 30311, A_ID)
```

Observed in 5 minutes on both nodes: repeated `already connected` collisions (6–7),
`BadMessage` applied to the trusted sibling at every establish (`reputation=-4096` and
falling), `backing off trusted peer` on 45s cadence. Extrapolation to the ban threshold
(`BANNED_REPUTATION = 50 * REPUTATION_UNIT`, trusted cap `2 * REPUTATION_UNIT`/hit):
~25 penalized reconnects ≈ 20 minutes → then the exact dev signature (0x00 kills at
establish). **Without** the flashblocks subprotocol, the same two-node setup resolves
the collision correctly and holds a stable session — vanilla reth mutual-trusted
peering is fine until something feeds reputation penalties.

## 5. Elimination record (what it is not)

- Not `eth_syncing`/gap-state: reproduces on a healthy, fully synced fleet.
- Not the peer monitor (`crates/p2p/src/monitor`): read-only, never mutates the network.
- Not `AlreadyConnected` mis-resolution alone: clean localhost run resolves it correctly.
- Not `remove_peer`/admin: trusted peers early-return there.
- Not `trusted_nodes_only`: not set; siblings are in `trusted_peer_ids`.
- 0x00-at-establish has exactly one reachable source given the above:
  `DisconnectBannedIncoming` (`state.rs` maps it to `reason: None` →
  `active.rs` defaults to `DisconnectRequested`).

## 6. The fix (reth, branch `fix/trusted-peer-ban-livelock`)

`crates/net/network/src/peers.rs`, one behavioral change: **always allow inbound
connections from banned trusted peers** — in `on_incoming_session_established`, both
rejection points recover instead of rejecting when the peer is trusted:

- `ban_list` membership (e.g. a temporary ban from a fatal connection error): lift the
  ban (`unban_peer`) and accept the session.
- entry-level `peer.is_banned()` (reputation below `BANNED_REPUTATION` — the path the
  builders actually hit): reset the reputation and accept. Without this the state is
  *unrecoverable by construction*: reth's `tick()` only heals reputation while a
  session is connected, and the ban prevents any session from connecting.

Why this is safe: every condition that justifies refusing a peer at this point
(genesis/chain/protocol mismatch) is re-validated during the handshake that just
completed — an inbound session can only establish once such a condition no longer
holds. For an operator-configured trusted peer, rejecting it here adds no protection;
it only surfaces as an opaque `DisconnectRequested` (0x00) on the dialer and, in a
mutual trusted mesh, produces fresh connection failures on both sides that sustain the
ban indefinitely.

Deliberately unchanged: `on_connection_failure` still bans trusted peers on
`is_fatal_protocol_error()` (upstream semantics — fatal means "do not keep redialing").
The trusted ban is short (`backoff_durations.low / 2`), and with the inbound-recovery
change above it can no longer wedge a mesh: the first successful inbound dial from the
peer clears it.

Tests added: `test_banned_trusted_peer_incoming_session_unbans`,
`test_reputation_banned_trusted_peer_incoming_session_resets`. Full `reth-network`
suite: 166/166. Diff: +95/−4, one file.

Also worth fixing upstream (not in this branch): `DisconnectBannedIncoming` /
`DisconnectUntrustedIncoming` map to `reason: None`, so the remote sees a meaningless
`DisconnectRequested`; a real reason code would have made this diagnosable from the
victim's logs in minutes.

## 7. Companion fix (world-chain, follow-up)

`crates/p2p`: make the flashblocks peer state connection-scoped (key the map by
connection/session id, or guard `on_peer_disconnected` so a stale connection's `Drop`
cannot remove a newer connection's entry), and **log every `reputation_change` the
subprotocol issues**. Optionally: don't penalize trusted peers for control-message
state races at all (`RequestFlashblocks` duplicates are benign).

## 8. Operational corroboration (crypto-dev, 2026-07-01)

- All 3 builders: siblings absent from `admin_peers` while healthy; kills every ~45s.
- Same behavior on both `v2.3.0` and `sha-9828474` images (both pin reth v2.3.0).
- Stage: same sibling absence, masked by ~20 public peers (pods up since Jun 17,
  started 5–12 min apart → less phase-locked churn; one long-lived flashblocks-carrying
  sibling session survives there, predating log retention).
- During the fleet incident, this livelock removed the only EL sync source available in
  dev, deadlocking recovery (see `INCIDENT-2026-07-01-builder-peering.md`).
