# Flashblocks Fanout Protocol v2

*This document specifies changes to the flashblocks P2P protocol (`flblk`) to replace full-broadcast fanout with bounded, latency-optimized peer selection. It is an amendment to the existing [Flashblocks P2P Extension](./specs/flashblocks_p2p.md).*

## Problem

The current flashblocks P2P protocol broadcasts every `FlashblocksPayloadV1` to **all** connected peers (`handler.rs:585`, `connection.rs:97-129`). A node with N peers sends N copies of every flashblock. For a node connected to 50 peers, that is 50x outgoing bandwidth per flashblock. As the network grows, this becomes unsustainable.

Additionally, `StartPublish` and `StopPublish` messages are currently **not relayed** beyond direct peers (see `connection.rs:343,436` TODOs). This must be addressed for multi-hop propagation to work correctly.

## Design Goals

1. **Reduce bandwidth** — Each node sends flashblocks to a bounded number of peers instead of all peers.
2. **Optimize latency** — Nodes periodically rotate out their slowest receive peers in favor of random alternatives.
3. **Reliability** — Multiple receive peers provide redundancy against individual peer failures.
4. **Trusted peer priority** — Trusted peers are always served when they request flashblocks, regardless of limits.
5. **Uniform roles** — Builders and consumers follow the same protocol. No special-casing by role.

## Overview

Each node maintains two bounded peer sets:

- **Send Set** (max `max_send_peers`, default 6): Peers this node actively forwards flashblocks to. These are peers that have sent a `RequestFlashblocks` and been accepted. Trusted peers bypass the limit.
- **Receive Set** (max `max_receive_peers`, default 6): Peers this node actively receives flashblocks from. These are peers to which this node has sent `RequestFlashblocks` and received `AcceptFlashblocks`.

Flashblocks propagate through the network as a directed acyclic graph: the builder sends to its send set, those nodes relay to their send sets, and so on. With a fanout of 6, a network of N nodes requires approximately log₆(N) hops from builder to the most distant node.

Periodically, each node evaluates the latency of its receive peers and may rotate out the highest-latency peer in favor of a randomly-selected alternative, one peer at a time.

## Protocol Version

This change adds new message types to the `flblk` protocol. The protocol version is bumped from `1` to `2`. Nodes advertising `flblk/2` support the fanout control messages described below. Nodes running `flblk/1` will not understand these messages and are incompatible.

## New Message Types

Five unsigned control messages are added to `FlashblocksP2PMsg`:

| Discriminator | Message | Direction | Description |
|---|---|---|---|
| `0x01` | `RequestFlashblocks` | Receiver → Sender | "I want to receive flashblocks from you" |
| `0x02` | `AcceptFlashblocks` | Sender → Receiver | "Accepted. I will send you flashblocks" |
| `0x03` | `RejectFlashblocks` | Sender → Receiver | "Rejected. I am at capacity" |
| `0x04` | `CancelFlashblocks` | Either → Either | "I am ending our flashblock feed" |
| `0x05` | `CancelFlashblocksAck` | Either → Either | "Acknowledged. Feed terminated" |

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
    CancelFlashblocksAck = 0x05,
}
```

### Message Semantics

**`RequestFlashblocks`** — Sent by a node that wants to receive flashblocks from the connected peer. The recipient evaluates:

1. Is the requester a trusted peer? → Always accept (trusted peers bypass `max_send_peers`).
2. Is the send set below `max_send_peers`? → Accept.
3. Is the send set full but contains non-trusted peers, AND the requester is trusted? → Evict a non-trusted peer (send it `CancelFlashblocks`), then accept.
4. Otherwise → Reject.

**`AcceptFlashblocks`** — Response to `RequestFlashblocks`. After this, the sender begins forwarding all `Authorized` messages to the receiver and adds the receiver to its send set.

**`RejectFlashblocks`** — Response to `RequestFlashblocks` when the sender cannot accommodate more peers. The requester should try another peer.

**`CancelFlashblocks`** — Either side may send this to terminate the flashblock feed:

- **Receiver-initiated**: "Stop sending me flashblocks." (e.g., during peer rotation)
- **Sender-initiated**: "I am going to stop sending you flashblocks." (e.g., evicting a non-trusted peer to make room for a trusted one)

The other party MUST respond with `CancelFlashblocksAck`.

**`CancelFlashblocksAck`** — Confirms the feed termination. After this exchange, both sides update their sets (sender removes from send set, receiver removes from receive set).

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

1. As peers connect and complete the `flblk/2` handshake, send `RequestFlashblocks` to them.
2. Prioritize trusted peers first.
3. Continue sending requests as new peers connect until `receive_set.len() >= max_receive_peers`.
4. Once the receive set is full, stop sending unsolicited requests (further changes happen via rotation).

### Handling Incoming `RequestFlashblocks`

```
receive RequestFlashblocks from peer P:

if P is trusted:
    if send_set has non-trusted peers AND send_set.len() >= max_send_peers:
        evict lowest-priority non-trusted peer (send CancelFlashblocks, await ack)
    add P to send_set
    send AcceptFlashblocks to P

else if send_set.len() < max_send_peers:
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
- **`StartPublish`**: Verify signatures, process (update publishing state machine). Forward to **all connected `flblk/2` peers** (not just send set). These are rare, small control messages needed by every node for multi-builder coordination.
- **`StopPublish`**: Same as `StartPublish` — forward to all connected peers.

If a node receives an `Authorized(FlashblocksPayloadV1)` from a peer **not** in its receive set, the message should be ignored. This prevents unsolicited data delivery.

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

This measurement is attributed to the specific peer that delivered the flashblock. Nodes maintain a sliding window of the last `latency_window` (default 50) measurements per receive peer and compute a moving average.

Since all receive peers deliver the same flashblock (with the same `flashblock_timestamp`), the **relative ordering** of peers by latency is accurate even with clock skew between the builder and receiver.

### Rotation Algorithm

**One-at-a-time rule**: Only one rotation may be in progress at any time. This ensures the receive set never drops below `max_receive_peers - 1` and allows the node to evaluate one change before making another.

During rotation, the node temporarily has `max_receive_peers + 1` receive peers (both the old and new peer are sending). This is intentional and provides a brief window to compare the two peers before committing to the switch.

### Rotation Timeout

If a rotation is in progress and no response (`AcceptFlashblocks`/`RejectFlashblocks`) is received within a reasonable timeout (e.g., 10 seconds), abort the rotation:

```
rotation_in_progress = false
remove R from pending_requests
```

## Configuration Parameters

| Parameter | Default | Description |
|---|---|---|
| `max_send_peers` | 6 | Maximum non-trusted peers to send flashblocks to |
| `max_receive_peers` | 6 | Maximum peers to receive flashblocks from |
| `rotation_interval` | 30s | How often to evaluate and potentially rotate receive peers |
| `latency_window` | 50 | Number of flashblocks to track for per-peer latency averaging |

Trusted peers are always served on request and **do not count** toward `max_send_peers`.

## Interaction with Existing Protocol

### Unchanged Components

The existing `Authorized` message types (`FlashblocksPayloadV1`, `StartPublish`, `StopPublish`) remain unchanged. They continue to use the `Authorized` wrapper with sequencer + builder signatures. The multi-builder coordination state machine (Publishing, WaitingToPublish, NotPublishing) is unaffected.

### Required Changes to Existing Code

1. **`StartPublish`/`StopPublish` must be forwarded** — The current code has TODOs at `connection.rs:343,436` noting these are not propagated. With multi-hop fanout, nodes more than 1 hop from the builder will never see these messages unless they are relayed. These must be forwarded to **all** connected `flblk/2` peers (not just send set) to ensure the multi-builder coordination works network-wide.

2. **Duplicate handling must change** — The current per-peer duplicate check at `connection.rs:278-291` penalizes any duplicate flashblock with `ReputationChangeKind::AlreadySeenTransaction`. In the new protocol, receiving the same flashblock from different receive peers is expected. Only same-peer duplicates (same flashblock index from the same peer twice) should trigger a penalty.

3. **Flashblock forwarding must be scoped to send set** — The current broadcast channel (`peer_tx`) sends to all connections. This must be replaced with targeted sends to only peers in the send set. The `PeerMsg::FlashblocksPayloadV1` variant currently uses a broadcast channel subscribed by all connections; this must be changed so each connection checks whether the destination peer is in the send set before forwarding.

4. **Protocol version bump** — `Capability::new_static("flblk", 1)` at `handler.rs:239` must be updated to version `2`.

