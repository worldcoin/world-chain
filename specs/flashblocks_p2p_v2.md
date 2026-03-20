# Flashblocks Fanout Protocol v2

*This document specifies changes to the flashblocks P2P protocol (`flblk`) to replace full-broadcast fanout with bounded, latency-optimized peer selection. It is an amendment to the existing [Flashblocks P2P Extension](./specs/flashblocks_p2p.md).*

## Problem

The current flashblocks P2P protocol broadcasts every `FlashblocksPayloadV1` to **all** connected peers (`handler.rs:585`, `connection.rs:97-129`). A node with N peers sends N copies of every flashblock. For a node connected to 50 peers, that is 50x outgoing bandwidth per flashblock. As the network grows, this becomes unsustainable.

## Design Goals

1. **Reduce bandwidth** — Each node sends flashblocks to a bounded number of peers instead of all peers.
2. **Optimize latency** — Nodes periodically rotate out their slowest receive peers in favor of random alternatives.
3. **Reliability** — Multiple receive peers provide redundancy against individual peer failures.
4. **Trusted peer priority** — Trusted peers are always served when they request flashblocks, regardless of limits.
5. **Uniform roles** — Builders and consumers follow the same protocol. No special-casing by role.

## Overview

Each node maintains two bounded peer sets:

- **Send Set** (max `max_send_peers`, default 10): Peers this node actively forwards flashblocks to. These are peers that have sent a `RequestFlashblocks` and been accepted. Trusted peers bypass the limit.
- **Receive Set** (max `max_receive_peers`, default 3): Peers this node actively receives flashblocks from. These are peers this node has selected as active feed sources.

Flashblocks propagate through the network as a directed acyclic graph: the builder sends to its send set, those nodes relay to their send sets, and so on. With a fanout of 10, a network of N nodes requires approximately log₁₀(N) hops from builder to the most distant node.

Periodically, each node evaluates the latency of its receive peers and may rotate out the highest-latency peer in favor of a randomly-selected alternative, one peer at a time.

## Protocol Version

This change adds new message types to the `flblk` protocol. The protocol version is bumped from `1` to `2`. Nodes advertising `flblk/2` support the fanout control messages described below. Nodes running `flblk/1` will not understand these messages and are incompatible.

## New Message Types

Four unsigned control messages are added to `FlashblocksP2PMsg`:

| Discriminator | Message | Direction | Description |
|---|---|---|---|
| `0x01` | `RequestFlashblocks` | Receiver → Sender | "I want to receive flashblocks from you" |
| `0x02` | `AcceptFlashblocks` | Sender → Receiver | "Accepted. I will send you flashblocks" |
| `0x03` | `RejectFlashblocks` | Sender → Receiver | "Rejected. I am at capacity" |
| `0x04` | `CancelFlashblocks` | Receiver → Sender | "Stop sending me flashblocks" |

These messages carry no payload. The connection context (peer ID) provides all necessary information.

```rust
pub enum FlashblocksP2PMsg {
    /// Existing authorized message wrapper (flashblock payloads, StartPublish, StopPublish).
    Authorized(Authorized) = 0x00,
    /// New fanout control messages (unsigned).
    RequestFlashblocks = 0x01,
    AcceptFlashblocks = 0x02,
    RejectFlashblocks = 0x03,
    CancelFlashblocks = 0x04,
}
```

### Message Semantics

**`RequestFlashblocks`** — Sent by a node that wants to receive flashblocks from the connected peer. The recipient evaluates:

1. Is the requester a trusted peer? → Always accept (trusted peers bypass `max_send_peers`).
2. Is the number of non-trusted peers in the send set below `max_send_peers`? → Accept.
3. Otherwise → Reject.

**`AcceptFlashblocks`** — Response to `RequestFlashblocks`. After this, the sender begins forwarding all `Authorized` messages to the receiver and adds the receiver to its send set.

**`RejectFlashblocks`** — Response to `RequestFlashblocks` when the sender cannot accommodate more peers. The requester should try another peer.

**`CancelFlashblocks`** — Sent only by a receiver to the sender it no longer wants to receive flashblocks from (e.g., during peer rotation).

After receiving `CancelFlashblocks`, the sender immediately stops forwarding flashblocks to that peer and removes it from its send set.

## Peer Management

### Per-Node State

```rust
struct FanoutState {
    /// Peers we are actively sending flashblocks to.
    send_set: HashSet<PeerId>,
    /// Peers we are actively receiving flashblocks from.
    receive_set: HashSet<PeerId>,
    /// Peers we have sent RequestFlashblocks to but not yet received a response.
    pending_requests: HashSet<PeerId>,
    /// Sliding-window latency stats per receive peer.
    peer_latency: HashMap<PeerId, LatencyTracker>,
    /// Whether a rotation is currently in progress.
    rotation_in_progress: bool,
}
```

### Bootstrapping (Node Startup)

When a node starts and connects to peers via devp2p:

1. As peers connect and complete the `flblk/2` handshake, discover whether they are trusted or untrusted.
2. Only request peers whose trust classification is known, so trusted peers are always considered first.
3. Continue sending requests as new peers connect until `receive_set.len() >= max_receive_peers`.
4. Once the receive set is full, stop sending unsolicited requests (further changes happen via rotation).

### Handling Incoming `RequestFlashblocks`

```
receive RequestFlashblocks from peer P:

if P is trusted:
    add P to send_set
    send AcceptFlashblocks to P

else if non_trusted_send_count < max_send_peers:
    add P to send_set
    send AcceptFlashblocks to P

else:
    send RejectFlashblocks to P
```

### Handling Disconnections

When a peer disconnects unexpectedly (connection drops):

- If peer was in **send set** → remove it. The slot is now available for future requests.
- If peer was in **receive set** → remove it. Immediately attempt to fill the slot by sending `RequestFlashblocks` to a random connected peer not already in `receive_set` or `pending_requests`.
- If peer was in **pending requests** → remove it. If receive set is not full, try another peer.

### Forwarding Rules

When a node receives an `Authorized` message from a peer in its receive set:

- **`FlashblocksPayloadV1`**: Verify signatures, process the flashblock (update state, emit to flashblock stream). Then forward the serialized bytes to all peers in the **send set** except the peer that sent it.
- **`StartPublish`**: Verify signatures and process locally. Do not relay it beyond the direct neighbor that sent it.
- **`StopPublish`**: Same as `StartPublish` — process locally, do not relay.

If a node receives an `Authorized(FlashblocksPayloadV1)` from a peer **not** in its receive set, or from a peer whose `RequestFlashblocks` is still pending, the message should be ignored and the peer should be penalized. This prevents unsolicited data delivery.

### Duplicate Handling

Since a node receives from up to `max_receive_peers` peers, it will receive multiple copies of each flashblock. This is expected behavior in the new protocol.

- **First copy**: Process normally (update state, emit to stream, forward to send set). Record latency for the delivering peer.
- **Subsequent copies (same flashblock from different peers)**: Record the latency for the delivering peer. Discard the flashblock data.
- **Same flashblock from the same peer twice**: This is NOT expected. Apply reputation penalty as before (`ReputationChangeKind::AlreadySeenTransaction`).

This is a behavioral change from the current protocol, where any duplicate from any peer triggers a reputation penalty (`connection.rs:288-290`).

## Latency-Based Peer Rotation

### Latency Measurement

Each `FlashblocksPayloadV1` includes a `flashblock_timestamp` in its metadata, set by the builder at creation time. When a node receives a flashblock from a receive peer, it computes:

```
one_way_latency = now() - flashblock_timestamp
```

This measurement is attributed to the specific peer that delivered the flashblock. Nodes maintain a sliding window of the last `latency_window` (default 1000) measurements per receive peer and compute a moving average.

Since all receive peers deliver the same flashblock (with the same `flashblock_timestamp`), the **relative ordering** of peers by latency is accurate even with clock skew between the builder and receiver.

### Rotation Algorithm

The receive set must never exceed `max_receive_peers`.

When rotating:

1. Select the worst-scoring peer in the current receive set.
2. Remove that peer from the receive set immediately and send `CancelFlashblocks`.
3. Pick a replacement candidate, prioritizing trusted peers.
4. Add the replacement peer to the receive set in a provisional state and send `RequestFlashblocks`.

The provisional peer occupies a receive slot immediately, so the node still never exceeds `max_receive_peers`. While provisional, the peer is scored for missed flashblocks the same as any other receive peer. If it fails to respond or fails to deliver flashblocks, its score will deteriorate and it can be rotated out on a later interval.

## Configuration Parameters

| Parameter | Default | Description |
|---|---|---|
| `max_send_peers` | 10 | Maximum non-trusted peers to send flashblocks to |
| `max_receive_peers` | 3 | Maximum peers to receive flashblocks from |
| `rotation_interval` | 30s | How often to evaluate and potentially rotate receive peers |
| `latency_window` | 1000 | Number of flashblocks to track for per-peer latency averaging |

Trusted peers are always served on request and **do not count** toward `max_send_peers`.

## Interaction with Existing Protocol

### Unchanged Components

The existing `Authorized` message types (`FlashblocksPayloadV1`, `StartPublish`, `StopPublish`) remain unchanged. They continue to use the `Authorized` wrapper with sequencer + builder signatures. The multi-builder coordination state machine (Publishing, WaitingToPublish, NotPublishing) is unaffected. `StartPublish` and `StopPublish` remain direct-neighbor messages and are not relayed.

### Required Changes to Existing Code

1. **Duplicate handling must change** — The current per-peer duplicate check at `connection.rs:278-291` penalizes any duplicate flashblock with `ReputationChangeKind::AlreadySeenTransaction`. In the new protocol, receiving the same flashblock from different receive peers is expected. Only same-peer duplicates (same flashblock index from the same peer twice) should trigger a penalty.

2. **Flashblock forwarding must be scoped to send set** — The current broadcast channel (`peer_tx`) sends to all connections. This must be replaced with targeted sends to only peers in the send set. The `PeerMsg::FlashblocksPayloadV1` variant currently uses a broadcast channel subscribed by all connections; this must be changed so each connection checks whether the destination peer is in the send set before forwarding.

3. **Receive-peer selection must respect trust discovery** — Nodes should not request unknown peers before their trust classification is available, otherwise untrusted peers can fill the bounded receive set before trusted peers are considered.

4. **Protocol version bump** — `Capability::new_static("flblk", 1)` at `handler.rs:239` must be updated to version `2`.
