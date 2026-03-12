use crate::protocol::{
    connection::{FlashblocksConnection, FlashblocksConnectionState, ReceiveStatus, Score},
    error::FlashblocksP2PError,
};
use alloy_rlp::BytesMut;
use chrono::Utc;
use ed25519_dalek::{SigningKey, VerifyingKey};
use flashblocks_cli::FanoutArgs;
use flashblocks_primitives::{
    p2p::{
        Authorization, Authorized, AuthorizedMsg, AuthorizedPayload, FlashblocksP2PMsg,
        StartPublish, StopPublish,
    },
    primitives::FlashblocksPayloadV1,
};
use futures::{Stream, StreamExt, stream};
use metrics::histogram;
use parking_lot::Mutex;
use rand::{Rng, seq::SliceRandom};
use reth::payload::PayloadId;
use reth_eth_wire::Capability;
use reth_ethereum::network::{api::PeerId, protocol::ProtocolHandler};
use reth_network::Peers;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{broadcast, mpsc, watch},
    time,
};
use tokio_stream::wrappers::BroadcastStream;
use tracing::{debug, info, warn};

use reth_ethereum::network::{
    api::Direction,
    eth_wire::{capability::SharedCapabilities, multiplex::ProtocolConnection, protocol::Protocol},
    protocol::{ConnectionHandler, OnNotSupported},
};
/// Maximum frame size for rlpx messages.
const MAX_FRAME: usize = 1 << 24; // 16 MiB

/// Maximum index for flashblocks payloads.
/// Not intended to ever be hit. Since we resize the flashblocks vector dynamically,
/// this is just a sanity check to prevent excessive memory usage.
pub(crate) const MAX_FLASHBLOCK_INDEX: usize = 100;

/// The maximum number of seconds we will wait for a previous publisher to stop
/// before continueing anyways.
const MAX_PUBLISH_WAIT_SEC: u64 = 2;

/// The maximum number of broadcast channel messages we will buffer
/// before dropping them. In practice, we should rarely need to buffer any messages.
const BROADCAST_BUFFER_CAPACITY: usize = 100;

/// A missed flashblock should dominate modest latency differences when rotating receive peers.
const MISSED_FLASHBLOCK_PENALTY_NS: i64 = 10_000_000_000;
/// Grace window in number of flashblocks to receive late flashblocks from peers before scoring them for missing flashblocks.
///
/// This must be at least long enough to cover AUTHORIZATION_TIMESTAMP_GRACE_SEC to prevent a spam
/// attack.
pub(crate) const RECEIVE_FLASHBLOCK_GRACE_WINDOW: usize = 50;

/// Maximum number of control messages (Request/Accept/Reject/Cancel) a peer may send
/// within a sliding window before being penalized.
const MAX_CONTROL_MSGS_PER_WINDOW: u32 = 10;

/// Duration of the per-peer control-message rate-limit window.
const CONTROL_MSG_WINDOW: Duration = Duration::from_secs(30);
/// Maximum time to wait for a peer to answer a `RequestFlashblocks` message.
const RECEIVE_REQUEST_TIMEOUT_SECS: u64 = 2;

/// Maximum time to wait for the network manager to expose the newly connected peer's trust info.
const PEER_INFO_LOOKUP_TIMEOUT: Duration = Duration::from_secs(1);

/// Poll interval while waiting for connected peer metadata to become available.
const PEER_INFO_LOOKUP_RETRY_INTERVAL: Duration = Duration::from_millis(10);

/// Trait bound for network handles that can be used with the flashblocks P2P protocol.
///
/// This trait combines all the necessary bounds for a network handle to be used
/// in the flashblocks P2P system, including peer management capabilities.
pub trait FlashblocksP2PNetworkHandle: Clone + Unpin + Peers + std::fmt::Debug + 'static {}

impl<N: Clone + Unpin + Peers + std::fmt::Debug + 'static> FlashblocksP2PNetworkHandle for N {}

/// The current publishing status of this node in the flashblocks P2P network.
///
/// This enum tracks whether we are actively publishing flashblocks, waiting to publish,
/// or not publishing at all. It also maintains information about other active publishers
/// to coordinate multi-builder scenarios and handle failover situations.
#[derive(Clone, Debug)]
pub enum PublishingStatus {
    /// We are currently publishing flashblocks.
    Publishing {
        /// The authorization token that grants us permission to publish.
        authorization: Authorization,
    },
    /// We are waiting for the previous publisher to stop.
    WaitingToPublish {
        /// The authorization token we will use once we start publishing.
        authorization: Authorization,
        /// A map of active publishers (excluding ourselves) to their most recently published
        /// or requested to publish block number.
        active_publishers: Vec<(VerifyingKey, u64)>,
    },
    /// We are not currently publishing flashblocks.
    NotPublishing {
        /// A map of previous publishers to their most recently published
        /// or requested to publish block number.
        active_publishers: Vec<(VerifyingKey, u64)>,
    },
}

impl Default for PublishingStatus {
    fn default() -> Self {
        Self::NotPublishing {
            active_publishers: Vec::new(),
        }
    }
}

/// Tracked information about a flashblock payload observed from the network.
#[derive(Clone, Debug)]
pub struct ObservedPayload {
    payload_id: PayloadId,
    timestamp: u64,
    flashblock_index: u64,
    /// Peers from which we've received this flashblock.
    received_peers: HashSet<PeerId>,
    /// Peers who we have sent this flashblock to.
    send_peers: HashSet<PeerId>,
}

/// Protocol state that stores the flashblocks P2P protocol events and coordination data.
///
/// This struct maintains the current state of flashblock publishing, including coordination
/// with other publishers, payload buffering, and ordering information. It serves as the
/// central state management for the flashblocks P2P protocol handler.
#[derive(Debug)]
pub struct FlashblocksP2PState {
    /// Current publishing status indicating whether we're publishing, waiting, or not publishing.
    pub publishing_status: watch::Sender<PublishingStatus>,
    /// Most recent payload ID for the current block being processed.
    pub payload_id: PayloadId,
    /// Timestamp of the most recent flashblocks payload.
    pub payload_timestamp: u64,
    /// Timestamp at which the most recent flashblock was received in ns since the unix epoch.
    pub flashblock_timestamp: i64,
    /// The index of the next flashblock to emit over the flashblocks stream.
    /// Used to maintain strict ordering of flashblock delivery.
    pub flashblock_index: usize,
    /// Buffer of flashblocks for the current payload, indexed by flashblock sequence number.
    /// Contains `None` for flashblocks not yet received, enabling out-of-order receipt
    /// while maintaining in-order delivery.
    pub flashblocks: Vec<Option<FlashblocksPayloadV1>>,
    /// Flashblocks observed from network peers, tracked until their receive grace windows expire.
    pub observed_payloads: VecDeque<ObservedPayload>,
    /// All currently connected peers and their connection state.
    pub connections: HashMap<PeerId, FlashblocksConnectionState>,
}

impl Default for FlashblocksP2PState {
    fn default() -> Self {
        let (publishing_status, _) = watch::channel(PublishingStatus::default());

        Self {
            publishing_status,
            payload_id: PayloadId::default(),
            payload_timestamp: 0,
            flashblock_timestamp: 0,
            flashblock_index: 0,
            flashblocks: Vec::new(),
            observed_payloads: VecDeque::new(),
            connections: HashMap::new(),
        }
    }
}

impl FlashblocksP2PState {
    /// Returns the connection state of a peer.
    pub(crate) fn connection_state(&self, peer_id: &PeerId) -> Option<&FlashblocksConnectionState> {
        self.connections.get(peer_id)
    }

    pub(crate) fn connection_state_mut(
        &mut self,
        peer_id: &PeerId,
    ) -> Option<&mut FlashblocksConnectionState> {
        self.connections.get_mut(peer_id)
    }

    /// Marks receiving a flashblock from a peer and returns whether this is the first time we've observed this peer receive this flashblock.
    ///
    /// Called when a flashblock is received from any peer.
    pub(crate) fn note_peer_received_flashblock(
        &mut self,
        authorization: &Authorization,
        flashblock: &FlashblocksPayloadV1,
        peer_id: PeerId,
    ) -> bool {
        if let Some(observed_payload) = self.observed_payloads.iter_mut().find(|observed_payload| {
            observed_payload.payload_id == flashblock.payload_id
                && observed_payload.flashblock_index == flashblock.index
        }) {
            return observed_payload.received_peers.insert(peer_id);
        }

        if self.observed_payloads.len() >= RECEIVE_FLASHBLOCK_GRACE_WINDOW {
            let evicted = self.observed_payloads.pop_front().unwrap();
            for (peer_id, connection) in &mut self.connections {
                if connection.receive_status_timestamp < evicted.timestamp + 2
                    && !evicted.received_peers.contains(peer_id)
                    && !evicted.send_peers.contains(peer_id)
                    && let ReceiveStatus::Receiving { score } = &mut connection.receive_status
                {
                    debug!(
                        target: "flashblocks::p2p",
                        %peer_id,
                        payload_id = %evicted.payload_id,
                        flashblock_index = evicted.flashblock_index,
                        "scoring peer for missed flashblock",
                    );
                    score.record(MISSED_FLASHBLOCK_PENALTY_NS);
                }
            }
        }

        self.observed_payloads.push_back(ObservedPayload {
            payload_id: flashblock.payload_id,
            timestamp: authorization.timestamp,
            flashblock_index: flashblock.index,
            received_peers: HashSet::from([peer_id]),
            send_peers: HashSet::new(),
        });

        true
    }

    /// Returns whether we've seen a given flashblock from a given peer.
    pub(crate) fn peer_received_flashblock(
        &self,
        peer_id: PeerId,
        payload_id: PayloadId,
        index: u64,
    ) -> bool {
        self.observed_payloads
            .iter()
            .find(|observed_payload| {
                observed_payload.payload_id == payload_id
                    && observed_payload.flashblock_index == index
            })
            .is_some_and(|observed_payload| observed_payload.received_peers.contains(&peer_id))
    }

    /// Sends an already serialized message to all connected peers.
    pub(crate) fn send_to_all_peers(&self, bytes: &BytesMut) {
        for conn in self.connections.values() {
            if let Some(tx) = &conn.outbound_tx {
                tx.send(bytes.clone()).ok();
            }
        }
    }

    /// Sends a serialized flashblock to peers in the current send set that have not
    /// already delivered that flashblock to us.
    fn send_flashblock_to_send_set(
        &mut self,
        payload_id: PayloadId,
        flashblock_index: u64,
        bytes: &BytesMut,
    ) {
        for (peer_id, conn) in &self.connections {
            if !conn.send_enabled
                || self.peer_received_flashblock(*peer_id, payload_id, flashblock_index)
            {
                continue;
            }
            self.observed_payloads
                .iter_mut()
                .find(|observed_payload| {
                    observed_payload.payload_id == payload_id
                        && observed_payload.flashblock_index == flashblock_index
                })
                .map(|observed_payload| observed_payload.send_peers.insert(*peer_id));

            if let Some(tx) = &conn.outbound_tx
                && tx.send(bytes.clone()).is_ok()
            {
                metrics::counter!("flashblocks.bandwidth_outbound").increment(bytes.len() as u64);
            }
        }
    }

    /// Sends a control message directly to a specific peer.
    fn send_direct(&self, peer_id: PeerId, msg: FlashblocksP2PMsg) {
        let bytes: &BytesMut = &msg.encode();
        if let Some(conn) = self.connections.get(&peer_id)
            && let Some(tx) = &conn.outbound_tx
        {
            tx.send(bytes.clone()).ok();
        }
    }

    /// Returns `true` if the peer has exceeded the control-message rate limit.
    fn check_control_rate_limit(&mut self, peer_id: &PeerId) -> bool {
        let Some(peer_state) = self.connections.get_mut(peer_id) else {
            return true;
        };
        let now = Instant::now();
        if now.duration_since(peer_state.control_msg_window_start) > CONTROL_MSG_WINDOW {
            peer_state.control_msg_count = 0;
            peer_state.control_msg_window_start = now;
        }
        peer_state.control_msg_count += 1;
        peer_state.control_msg_count > MAX_CONTROL_MSGS_PER_WINDOW
    }

    fn num_receive_peers(&self) -> usize {
        self.connections
            .values()
            .filter(|peer_state| {
                matches!(peer_state.receive_status, ReceiveStatus::Receiving { .. })
            })
            .count()
    }

    fn receive_retry_cooldown_secs(ctx: &FlashblocksP2PCtx) -> u64 {
        Duration::from_secs(ctx.fanout_args.rotation_interval)
            .as_secs()
            .max(1)
    }

    fn clear_receive_state(
        peer_state: &mut FlashblocksConnectionState,
        receive_status_timestamp: u64,
    ) {
        peer_state.receive_status = ReceiveStatus::NotReceiving;
        peer_state.receive_status_timestamp = receive_status_timestamp;
    }

    fn available_receive_candidates(&self, ctx: &FlashblocksP2PCtx) -> Vec<(PeerId, bool)> {
        let now = Utc::now().timestamp() as u64;
        let retry_cooldown = Self::receive_retry_cooldown_secs(ctx);
        self.connections
            .iter()
            .filter_map(|(peer_id, peer_state)| {
                if peer_state.receive_status == ReceiveStatus::NotReceiving
                    && (peer_state.receive_status_timestamp == 0
                        || peer_state.receive_status_timestamp + retry_cooldown <= now)
                {
                    Some((*peer_id, peer_state.trusted))
                } else {
                    None
                }
            })
            .collect()
    }

    fn begin_requesting_peer(&mut self, _ctx: &FlashblocksP2PCtx, peer_id: PeerId) {
        let Some(peer_state) = self.connection_state_mut(&peer_id) else {
            return;
        };
        let timestamp = Utc::now().timestamp() as u64;
        peer_state.receive_status = ReceiveStatus::Requesting;
        peer_state.receive_status_timestamp = timestamp;
        self.send_direct(peer_id, FlashblocksP2PMsg::RequestFlashblocks);
    }

    fn num_receive_or_requesting_peers(&self) -> usize {
        self.connections
            .values()
            .filter(|peer_state| {
                matches!(
                    peer_state.receive_status,
                    ReceiveStatus::Receiving { .. } | ReceiveStatus::Requesting
                )
            })
            .count()
    }

    pub fn maybe_request_receive_peers(&mut self, ctx: &FlashblocksP2PCtx) {
        while self.num_receive_or_requesting_peers() < ctx.fanout_args.max_receive_peers {
            let candidates = self.available_receive_candidates(ctx);
            if candidates.is_empty() {
                return;
            }
            let trusted_candidates: Vec<_> = candidates
                .iter()
                .filter_map(|(peer_id, trusted)| (*trusted).then_some(*peer_id))
                .collect();
            let candidate_pool = if trusted_candidates.is_empty() {
                candidates
                    .iter()
                    .map(|(peer_id, _)| *peer_id)
                    .collect::<Vec<_>>()
            } else {
                trusted_candidates
            };
            let rand = rand::rng().random_range(0..candidate_pool.len());
            self.begin_requesting_peer(ctx, candidate_pool[rand]);
        }
    }

    fn expire_stale_receive_requests(&mut self, ctx: &FlashblocksP2PCtx) {
        let now = Utc::now().timestamp() as u64;
        let mut cleared_any = false;

        for peer_state in self.connections.values_mut() {
            if matches!(peer_state.receive_status, ReceiveStatus::Requesting)
                && peer_state.receive_status_timestamp + RECEIVE_REQUEST_TIMEOUT_SECS <= now
            {
                Self::clear_receive_state(peer_state, now);
                cleared_any = true;
            }
        }

        if cleared_any {
            self.maybe_request_receive_peers(ctx);
        }
    }

    fn worst_receive_peer(&self) -> Option<PeerId> {
        self.connections
            .iter()
            .filter_map(|(peer_id, peer_state)| {
                let ReceiveStatus::Receiving { score } = &peer_state.receive_status else {
                    return None;
                };
                Some((*peer_id, score.value()))
            })
            .max_by(
                |(_, lhs_score), (_, rhs_score)| match (lhs_score, rhs_score) {
                    (None, None) => std::cmp::Ordering::Equal,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (Some(lhs), Some(rhs)) => lhs.cmp(rhs),
                },
            )
            .map(|(peer_id, _)| peer_id)
    }

    fn maybe_start_rotation(&mut self, ctx: &FlashblocksP2PCtx) {
        if self.num_receive_peers() < ctx.fanout_args.max_receive_peers {
            return;
        }

        let Some(evict) = self.worst_receive_peer() else {
            return;
        };

        let mut candidates = self.available_receive_candidates(ctx);
        if candidates.is_empty() {
            return;
        }

        let mut rng = rand::rng();
        candidates.shuffle(&mut rng);
        let candidate = candidates
            .iter()
            .find_map(|(peer_id, trusted)| (*trusted).then_some(*peer_id))
            .unwrap_or(candidates[0].0);

        let evict_timestamp = Utc::now().timestamp() as u64;
        if let Some(evict_state) = self.connection_state_mut(&evict) {
            Self::clear_receive_state(evict_state, evict_timestamp);
        }
        self.send_direct(evict, FlashblocksP2PMsg::CancelFlashblocks);

        self.begin_requesting_peer(ctx, candidate);
    }

    /// Returns `true` if the peer should receive a reputation penalty.
    fn handle_request(&mut self, ctx: &FlashblocksP2PCtx, peer_id: PeerId) -> bool {
        if self.check_control_rate_limit(&peer_id) {
            return true;
        }

        let Some(peer_state) = self.connection_state(&peer_id) else {
            return false;
        };

        if peer_state.send_enabled {
            // Already sending to this peer — repeated request is spam.
            return true;
        }
        let peer_is_trusted = peer_state.trusted;
        let non_trusted_send_count = self
            .connections
            .values()
            .filter(|s| s.send_enabled && !s.trusted)
            .count();

        if !peer_is_trusted && non_trusted_send_count >= ctx.fanout_args.max_send_peers {
            self.send_direct(peer_id, FlashblocksP2PMsg::RejectFlashblocks);
            return false;
        }

        let peer_state = self.connection_state_mut(&peer_id).expect("peer exists");
        peer_state.send_enabled = true;
        self.send_direct(peer_id, FlashblocksP2PMsg::AcceptFlashblocks);
        false
    }

    /// Returns `true` if the peer should receive a reputation penalty.
    fn handle_accept(&mut self, ctx: &FlashblocksP2PCtx, peer_id: PeerId) -> bool {
        if self.check_control_rate_limit(&peer_id) {
            return true;
        }

        let Some(peer_state) = self.connection_state_mut(&peer_id) else {
            return false;
        };

        match peer_state.receive_status {
            ReceiveStatus::Requesting => {
                peer_state.receive_status = ReceiveStatus::Receiving {
                    score: Score::new(ctx.fanout_args.score_samples),
                };
                false
            }
            // Unsolicited accept — we never asked this peer.
            _ => true,
        }
    }

    /// Returns `true` if the peer should receive a reputation penalty.
    fn handle_reject(&mut self, ctx: &FlashblocksP2PCtx, peer_id: PeerId) -> bool {
        if self.check_control_rate_limit(&peer_id) {
            return true;
        }

        let Some(peer_state) = self.connection_state_mut(&peer_id) else {
            return false;
        };

        match peer_state.receive_status {
            ReceiveStatus::Requesting => {
                Self::clear_receive_state(peer_state, Utc::now().timestamp() as u64);
                self.maybe_request_receive_peers(ctx);
                false
            }
            // Unsolicited reject — we never asked this peer.
            _ => true,
        }
    }

    /// Returns `true` if the peer should receive a reputation penalty.
    fn handle_cancel(&mut self, _ctx: &FlashblocksP2PCtx, peer_id: PeerId) -> bool {
        if self.check_control_rate_limit(&peer_id) {
            return true;
        }

        let Some(peer_state) = self.connection_state_mut(&peer_id) else {
            return false;
        };

        if !peer_state.send_enabled {
            // Cancel is only valid from a receiver to its sender.
            return true;
        }

        peer_state.send_enabled = false;
        false
    }
}

/// Context struct containing shared resources for the flashblocks P2P protocol.
///
/// This struct holds the network handle, cryptographic keys, and communication channels
/// used across all connections in the flashblocks P2P protocol. It provides the shared
/// infrastructure needed for message verification, signing, and broadcasting.
#[derive(Clone, Debug)]
pub struct FlashblocksP2PCtx {
    /// Authorizer's verifying key used to verify authorization signatures from rollup-boost.
    pub authorizer_vk: VerifyingKey,
    /// Flashblocks configuration including signing keys and fanout args.
    pub fanout_args: FanoutArgs,
    /// Broadcast sender for verified and strictly ordered flashblock payloads.
    /// Used by RPC overlays and other consumers of flashblock data.
    pub flashblock_tx: broadcast::Sender<FlashblocksPayloadV1>,
}

/// Handle for the flashblocks P2P protocol.
///
/// Encapsulates the shared context and mutable state of the flashblocks
/// P2P protocol.
#[derive(Clone, Debug)]
pub struct FlashblocksHandle {
    /// Shared context containing network handle, keys, and communication channels.
    pub ctx: FlashblocksP2PCtx,
    /// Builder signing key used to sign outgoing authorized P2P messages.
    pub builder_sk: Option<SigningKey>,
    /// Thread-safe mutable state of the flashblocks protocol.
    /// Protected by a mutex to allow concurrent access from multiple connections.
    pub state: Arc<Mutex<FlashblocksP2PState>>,
}

impl FlashblocksHandle {
    pub fn new(authorizer_vk: VerifyingKey, builder_sk: Option<SigningKey>) -> Self {
        Self::with_fanout_args(authorizer_vk, builder_sk, FanoutArgs::default())
    }

    pub fn with_fanout_args(
        authorizer_vk: VerifyingKey,
        builder_sk: Option<SigningKey>,
        fanout_args: FanoutArgs,
    ) -> Self {
        let flashblock_tx = broadcast::Sender::new(BROADCAST_BUFFER_CAPACITY);
        let state = Arc::new(Mutex::new(FlashblocksP2PState::default()));
        let ctx = FlashblocksP2PCtx {
            authorizer_vk,
            fanout_args,
            flashblock_tx,
        };
        let handle = Self {
            ctx,
            builder_sk,
            state,
        };
        let moved_handle = handle.clone();

        tokio::spawn(async move {
            let mut rotation_interval = time::interval(Duration::from_secs(
                moved_handle.ctx.fanout_args.rotation_interval,
            ));
            rotation_interval.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
            rotation_interval.tick().await;

            loop {
                rotation_interval.tick().await;
                let mut state = moved_handle.state.lock();
                state.expire_stale_receive_requests(&moved_handle.ctx);
                state.maybe_request_receive_peers(&moved_handle.ctx);
                state.maybe_start_rotation(&moved_handle.ctx);
            }
        });

        handle
    }

    pub(crate) fn on_peer_connected<N: FlashblocksP2PNetworkHandle>(
        &self,
        network: N,
        peer_id: PeerId,
        outbound_tx: mpsc::UnboundedSender<BytesMut>,
    ) {
        let trusted = tokio::task::block_in_place(|| {
            let network = network.clone();
            tokio::runtime::Handle::current().block_on(async move {
                let deadline = Instant::now() + PEER_INFO_LOOKUP_TIMEOUT;

                loop {
                    match network.get_peer_by_id(peer_id).await {
                        Ok(Some(peer_info)) => return Ok(peer_info.kind.is_trusted()),
                        Ok(None) if Instant::now() < deadline => {
                            time::sleep(PEER_INFO_LOOKUP_RETRY_INTERVAL).await;
                        }
                        Ok(None) => {
                            return Err(
                                "timed out waiting for peer info after connection".to_owned()
                            );
                        }
                        Err(error) if Instant::now() < deadline => {
                            time::sleep(PEER_INFO_LOOKUP_RETRY_INTERVAL).await;
                            tracing::debug!(
                                target: "flashblocks::p2p",
                                %peer_id,
                                %error,
                                "retrying peer info lookup for flashblocks fanout"
                            );
                        }
                        Err(error) => {
                            return Err(format!(
                                "failed to load peer info for flashblocks fanout: {error}"
                            ));
                        }
                    }
                }
            })
        });

        let trusted = match trusted {
            Ok(trusted) => trusted,
            Err(error) => {
                warn!(
                    target: "flashblocks::p2p",
                    %peer_id,
                    %error,
                    "failed to classify peer for flashblocks fanout; defaulting to untrusted"
                );
                false
            }
        };

        let mut state = self.state.lock();
        let mut conn_state = FlashblocksConnectionState::new();
        conn_state.outbound_tx = Some(outbound_tx);
        conn_state.trusted = trusted;
        state.connections.insert(peer_id, conn_state);
        state.maybe_request_receive_peers(&self.ctx);
    }

    pub(crate) fn on_peer_disconnected(&self, peer_id: PeerId) {
        let mut state = self.state.lock();
        state.connections.remove(&peer_id);
        state.maybe_request_receive_peers(&self.ctx);
    }

    /// Returns `true` if the peer should receive a reputation penalty.
    pub(crate) fn handle_request_message(&self, peer_id: PeerId) -> bool {
        let mut state = self.state.lock();
        state.handle_request(&self.ctx, peer_id)
    }

    /// Returns `true` if the peer should receive a reputation penalty.
    pub(crate) fn handle_accept_message(&self, peer_id: PeerId) -> bool {
        let mut state = self.state.lock();
        state.handle_accept(&self.ctx, peer_id)
    }

    /// Returns `true` if the peer should receive a reputation penalty.
    pub(crate) fn handle_reject_message(&self, peer_id: PeerId) -> bool {
        let mut state = self.state.lock();
        state.handle_reject(&self.ctx, peer_id)
    }

    /// Returns `true` if the peer should receive a reputation penalty.
    pub(crate) fn handle_cancel_message(&self, peer_id: PeerId) -> bool {
        let mut state = self.state.lock();
        state.handle_cancel(&self.ctx, peer_id)
    }
}

/// Main protocol handler for the flashblocks P2P protocol.
///
/// # Important
///
/// You must call `NetworkBuilder::add_rlpx_sub_protocol` to register the rlpx sub protocol
/// _before_ starting the network to avoid a race condition where trusted peers are connected
/// before the protocol is live.
///
/// This handler manages incoming and outgoing connections, coordinates flashblock publishing,
/// and maintains the protocol state across all peer connections. It implements the core
/// logic for multi-builder coordination and failover scenarios in HA sequencer setups.
#[derive(Clone, Debug)]
pub struct FlashblocksP2PProtocol<N> {
    /// Network handle used to update peer reputation and manage connections.
    pub network: N,
    /// Shared context containing network handle, keys, and communication channels.
    pub handle: FlashblocksHandle,
}

impl<N: FlashblocksP2PNetworkHandle> FlashblocksP2PProtocol<N> {
    /// Creates a new flashblocks P2P protocol handler.
    ///
    /// Initializes the handler with the necessary cryptographic keys, network handle,
    /// and communication channels. The handler starts in a non-publishing state.
    ///
    /// # Arguments
    /// * `network` - Network handle for peer management and reputation updates
    /// * `handle` - Shared handle containing the protocol context and mutable state
    pub fn new(network: N, handle: FlashblocksHandle) -> Self {
        Self {
            network: network.clone(),
            handle,
        }
    }
}

impl<N> FlashblocksP2PProtocol<N> {
    /// Returns the P2P capability for the flashblocks v2 protocol.
    ///
    /// This capability is used during devp2p handshake to advertise support
    /// for the flashblocks protocol with protocol name "flblk" and version 2.
    pub fn capability() -> Capability {
        Capability::new_static("flblk", 2)
    }
}

impl FlashblocksHandle {
    /// Retrieves the next flashblock from the protocol state based on the provided cursor.
    ///
    /// Will return the flashblock at the cursor if it exists.
    /// Will return the first flashblock if the cursor points to a different payload or is None.
    /// Returns None if the flashblock at the cursor or the first flashblock does not exist.
    fn next_flashblock_from_state(
        state: &FlashblocksP2PState,
        cursor: Option<&(PayloadId, usize)>,
    ) -> Option<FlashblocksPayloadV1> {
        match cursor {
            Some((payload_id, next_index)) if *payload_id == state.payload_id => state
                .flashblocks
                .get(*next_index)
                .and_then(|flashblock| flashblock.clone()),
            _ => state
                .flashblocks
                .first()
                .and_then(|flashblock| flashblock.clone()),
        }
    }

    /// Returns the builder signing key if configured.
    pub fn builder_sk(&self) -> Result<&SigningKey, FlashblocksP2PError> {
        self.builder_sk
            .as_ref()
            .ok_or(FlashblocksP2PError::MissingBuilderSk)
    }

    /// Publishes a newly created flashblock from the payload builder to the P2P network.
    ///
    /// This method validates that the builder has authorization to publish and that
    /// the authorization matches the current publishing session. The flashblock is
    /// then processed, cached, and forwarded to peers in the current send set.
    ///
    /// # Arguments
    /// * `authorized_payload` - The signed flashblock payload with authorization
    ///
    /// # Returns
    /// * `Ok(())` if the flashblock was successfully published
    /// * `Err` if the builder lacks authorization or the authorization is outdated
    ///
    /// # Note
    /// You must call `start_publishing` before calling this method to establish
    /// authorization for the current block.
    pub fn publish_new(
        &self,
        authorized_payload: AuthorizedPayload<FlashblocksPayloadV1>,
    ) -> Result<(), FlashblocksP2PError> {
        let mut state = self.state.lock();
        let PublishingStatus::Publishing { authorization } = *state.publishing_status.borrow()
        else {
            return Err(FlashblocksP2PError::NotClearedToPublish);
        };

        if authorization != authorized_payload.authorized.authorization {
            return Err(FlashblocksP2PError::ExpiredAuthorization);
        }
        self.ctx.publish(&mut state, authorized_payload);
        Ok(())
    }

    /// Sends an already serialized protocol message to all currently connected peers.
    pub fn send_serialized_to_all_peers(&self, bytes: BytesMut) {
        self.state.lock().send_to_all_peers(&bytes);
    }

    /// Returns the current publishing status of this node.
    ///
    /// The status indicates whether the node is actively publishing flashblocks,
    /// waiting for another publisher to stop, or not publishing at all.
    ///
    /// # Returns
    /// The current `PublishingStatus` enum value
    pub fn publishing_status(&self) -> PublishingStatus {
        self.state.lock().publishing_status.borrow().clone()
    }

    /// Awaits clearance to publish flashblocks.
    ///
    /// # Note
    /// This is never guaranteed to return.
    pub async fn await_clearance(&self) {
        let mut status = self.state.lock().publishing_status.subscribe();

        // Safe to unwrap becuase self holds a sender.
        status
            .wait_for(|status| matches!(status, PublishingStatus::Publishing { .. }))
            .await
            .unwrap();
    }

    /// Initiates flashblock publishing for a new block.
    ///
    /// This method should be called immediately after receiving a ForkChoiceUpdated
    /// with payload attributes and the corresponding Authorization token. It coordinates
    /// with other potential publishers to ensure only one builder publishes at a time.
    ///
    /// The method may transition the node to either Publishing or WaitingToPublish state
    /// depending on whether other builders are currently active.
    ///
    /// # Arguments
    /// * `new_authorization` - Authorization token signed by rollup-boost for this block
    ///
    /// # Note
    /// Calling this method does not guarantee immediate publishing clearance.
    /// The node may need to wait for other publishers to stop first.
    pub fn start_publishing(
        &self,
        new_authorization: Authorization,
    ) -> Result<(), FlashblocksP2PError> {
        let state = self.state.lock();
        let builder_sk = self.builder_sk()?;
        state.publishing_status.send_modify(|status| {
            match status {
                PublishingStatus::Publishing { authorization } => {
                    // Ensure that the new authorization's timestamp is strictly greater than the currently
                    // active one before updating, otherwise it would be possible to reuse outdated authorizations
                    // for an active publisher.
                    if new_authorization.timestamp > authorization.timestamp {
                        // We are already publishing, and the new authorization is newer than the
                        // current active one, so we just update the authorization.
                        *authorization = new_authorization;
                    }
                }
                PublishingStatus::WaitingToPublish {
                    authorization,
                    active_publishers,
                } => {
                    let most_recent_publisher = active_publishers
                        .iter()
                        .map(|(_, timestamp)| *timestamp)
                        .max()
                        .unwrap_or_default();
                    // We are waiting to publish, so we update the authorization and
                    // the block number at which we requested to start publishing.
                    if new_authorization.timestamp >= most_recent_publisher + MAX_PUBLISH_WAIT_SEC {
                        // If the block number is greater than the one we requested to start publishing,
                        // we will update it.
                        tracing::warn!(
                            target: "flashblocks::p2p",
                            payload_id = %new_authorization.payload_id,
                            timestamp = %new_authorization.timestamp,
                            "waiting to publish timed out, starting to publish",
                        );
                        *status = PublishingStatus::Publishing {
                            authorization: new_authorization,
                        };
                    } else {
                        // Continue to wait for the previous builder to stop.
                        *authorization = new_authorization;
                    }
                }
                PublishingStatus::NotPublishing { active_publishers } => {
                    // Send an authorized `StartPublish` message to the network
                    let authorized_msg = AuthorizedMsg::StartPublish(StartPublish);
                    let authorized_payload =
                        Authorized::new(builder_sk, new_authorization, authorized_msg);
                    let p2p_msg = FlashblocksP2PMsg::Authorized(authorized_payload);
                    state.send_to_all_peers(&p2p_msg.encode());

                    if active_publishers.is_empty() {
                        // If we have no previous publishers, we can start publishing immediately.
                        tracing::info!(
                            target: "flashblocks::p2p",
                            payload_id = %new_authorization.payload_id,
                            "starting to publish flashblocks",
                        );
                        *status = PublishingStatus::Publishing {
                            authorization: new_authorization,
                        };
                    } else {
                        // If we have previous publishers, we will wait for them to stop.
                        tracing::info!(
                            target: "flashblocks::p2p",
                            payload_id = %new_authorization.payload_id,
                            "waiting to publish flashblocks",
                        );
                        *status = PublishingStatus::WaitingToPublish {
                            authorization: new_authorization,
                            active_publishers: active_publishers.clone(),
                        };
                    }
                }
            }
        });

        Ok(())
    }

    /// Stops flashblock publishing and notifies the P2P network.
    ///
    /// This method broadcasts a StopPublish message to all connected peers and transitions
    /// the node to a non-publishing state. It should be called when receiving a
    /// ForkChoiceUpdated without payload attributes or without an Authorization token.
    pub fn stop_publishing(&self) -> Result<(), FlashblocksP2PError> {
        let state = self.state.lock();
        let builder_sk = self.builder_sk()?;

        state.publishing_status.send_modify(|status| {
            match status {
                PublishingStatus::Publishing { authorization } => {
                    // We are currently publishing, so we send a stop message.
                    tracing::info!(
                        target: "flashblocks::p2p",
                        payload_id = %authorization.payload_id,
                        timestamp = %authorization.timestamp,
                        "stopping to publish flashblocks",
                    );
                    let authorized_payload =
                        Authorized::new(builder_sk, *authorization, StopPublish.into());
                    let p2p_msg = FlashblocksP2PMsg::Authorized(authorized_payload);
                    state.send_to_all_peers(&p2p_msg.encode());
                    *status = PublishingStatus::NotPublishing {
                        active_publishers: Vec::new(),
                    };
                }
                PublishingStatus::WaitingToPublish {
                    authorization,
                    active_publishers,
                    ..
                } => {
                    // We are waiting to publish, so we just update the status.
                    tracing::info!(
                        target: "flashblocks::p2p",
                        payload_id = %authorization.payload_id,
                        timestamp = %authorization.timestamp,
                        "aborting wait to publish flashblocks",
                    );
                    let authorized_payload =
                        Authorized::new(builder_sk, *authorization, StopPublish.into());
                    let p2p_msg = FlashblocksP2PMsg::Authorized(authorized_payload);
                    state.send_to_all_peers(&p2p_msg.encode());
                    *status = PublishingStatus::NotPublishing {
                        active_publishers: active_publishers.clone(),
                    };
                }
                PublishingStatus::NotPublishing { .. } => {}
            }
        });

        Ok(())
    }

    /// Returns a stream of ordered flashblocks starting from the beginning of the current payload.
    ///
    /// # Behavior
    /// The stream will continue to yield flashblocks for consecutive payloads.
    pub fn flashblock_stream(&self) -> impl Stream<Item = FlashblocksPayloadV1> + Send + 'static {
        // Seed the stream with already-buffered contiguous flashblocks, then rely on the broadcast
        // channel for future ones so ordering stays strict even if inserts arrive out of order.
        let flashblocks = self
            .state
            .lock()
            .flashblocks
            .clone()
            .into_iter()
            .map_while(|x| x);

        let receiver = self.ctx.flashblock_tx.subscribe();

        let current = stream::iter(flashblocks);
        let future = tokio_stream::StreamExt::map_while(BroadcastStream::new(receiver), |x| x.ok());
        current.chain(future)
    }

    /// Returns a stream of ordered flashblocks starting from the beginning of the current payload.
    ///
    /// # Behavior
    ///
    /// The stream will continue to yield flashblocks for consecutive payloads.
    ///
    /// Items not consumed from the stream by the time the next payload starts will be skipped.
    pub fn live_flashblock_stream(
        &self,
    ) -> impl Stream<Item = FlashblocksPayloadV1> + Send + Unpin + 'static {
        let state = self.state.clone();
        let receiver = self.ctx.flashblock_tx.subscribe();

        Box::pin(stream::unfold(
            (state, receiver, None::<(PayloadId, usize)>),
            |(state, mut receiver, mut cursor)| async move {
                loop {
                    if let Some(flashblock) = {
                        let state = state.lock();
                        Self::next_flashblock_from_state(&state, cursor.as_ref())
                    } {
                        cursor = Some((flashblock.payload_id, flashblock.index as usize + 1));
                        return Some((flashblock, (state, receiver, cursor)));
                    }

                    match receiver.recv().await {
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(
                                target: "flashblocks::p2p",
                                skipped,
                                "flashblock stream lagged; resyncing from protocol state"
                            );
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                    }
                }
            },
        ))
    }
}

impl FlashblocksP2PCtx {
    /// Processes and publishes a verified flashblock payload to the P2P network and local stream.
    ///
    /// This method handles the core logic of flashblock processing, including validation,
    /// caching, and broadcasting. It ensures flashblocks are delivered in order while
    /// allowing out-of-order receipt from the network.
    ///
    /// # Arguments
    /// * `state` - Mutable reference to the protocol state for updating flashblock cache
    /// * `authorized_payload` - The authorized flashblock payload to process and publish
    ///
    /// # Behavior
    /// - Validates payload consistency with authorization
    /// - Updates global state for new payloads with newer timestamps
    /// - Caches flashblocks and maintains ordering for sequential delivery
    /// - Forwards flashblocks to peers in the current send set and publishes ordered
    ///   flashblocks to the local stream
    pub fn publish(
        &self,
        state: &mut FlashblocksP2PState,
        authorized_payload: AuthorizedPayload<FlashblocksPayloadV1>,
    ) {
        let payload = authorized_payload.msg();
        let authorization = authorized_payload.authorized.authorization;

        // Do some basic validation
        if authorization.payload_id != payload.payload_id {
            // Since the builders are trusted, the only reason this should happen is a bug.
            tracing::error!(
                target: "flashblocks::p2p",
                authorization_payload_id = %authorization.payload_id,
                flashblock_payload_id = %payload.payload_id,
                "Authorization payload id does not match flashblocks payload id"
            );
            return;
        }

        // Check if this is a globally new payload
        if authorization.timestamp > state.payload_timestamp {
            state.payload_id = authorization.payload_id;
            state.payload_timestamp = authorization.timestamp;
            state.flashblock_index = 0;
            state.flashblocks.fill(None);
        }

        // Resize our array if needed
        if payload.index as usize > MAX_FLASHBLOCK_INDEX {
            tracing::error!(
                target: "flashblocks::p2p",
                index = payload.index,
                max_index = MAX_FLASHBLOCK_INDEX,
                "Received flashblocks payload with index exceeding maximum"
            );
            return;
        }
        let len = state.flashblocks.len();
        state
            .flashblocks
            .resize_with(len.max(payload.index as usize + 1), || None);
        let flashblock = &mut state.flashblocks[payload.index as usize];

        // If we've already seen this index, skip it
        // Otherwise, add it to the list
        if flashblock.is_none() {
            // We haven't seen this index yet
            // Add the flashblock to our cache

            *flashblock = Some(payload.clone());
            tracing::trace!(
                target: "flashblocks::p2p",
                payload_id = %payload.payload_id,
                flashblock_index = payload.index,
                "queueing flashblock",
            );

            let p2p_msg = FlashblocksP2PMsg::Authorized(authorized_payload.authorized.clone());
            let bytes = p2p_msg.encode();
            let len = bytes.len();

            if len > MAX_FRAME {
                tracing::error!(
                    target: "flashblocks::p2p",
                    size = bytes.len(),
                    max_size = MAX_FRAME,
                    "FlashblocksP2PMsg too large",
                );
                return;
            }
            if len > MAX_FRAME / 2 {
                tracing::warn!(
                    target: "flashblocks::p2p",
                    size = bytes.len(),
                    max_size = MAX_FRAME,
                    "FlashblocksP2PMsg almost too large",
                );
            }

            metrics::histogram!("flashblocks.size").record(len as f64);
            metrics::histogram!("flashblocks.gas_used").record(payload.diff.gas_used as f64);
            metrics::histogram!("flashblocks.tx_count")
                .record(payload.diff.transactions.len() as f64);

            state.send_flashblock_to_send_set(payload.payload_id, payload.index, &bytes);

            let now = Utc::now()
                .timestamp_nanos_opt()
                .expect("time went backwards");

            // Broadcast any flashblocks in the cache that are in order
            while let Some(Some(flashblock_event)) = state.flashblocks.get(state.flashblock_index) {
                // Publish the flashblock
                debug!(
                    target: "flashblocks::p2p",
                    payload_id = %flashblock_event.payload_id,
                    flashblock_index = %state.flashblock_index,
                    "publishing flashblock"
                );
                self.flashblock_tx.send(flashblock_event.clone()).ok();

                // Don't measure the interval at the block boundary
                if state.flashblock_index != 0 {
                    let interval = now - state.flashblock_timestamp;
                    histogram!("flashblocks.interval").record(interval as f64 / 1_000_000_000.0);
                }

                // Update the index and timestamp
                state.flashblock_timestamp = now;
                state.flashblock_index += 1;
            }
        }
    }
}

impl<N: FlashblocksP2PNetworkHandle> ProtocolHandler for FlashblocksP2PProtocol<N> {
    type ConnectionHandler = Self;

    fn on_incoming(&self, _socket_addr: SocketAddr) -> Option<Self::ConnectionHandler> {
        Some(self.clone())
    }

    fn on_outgoing(
        &self,
        _socket_addr: SocketAddr,
        _peer_id: PeerId,
    ) -> Option<Self::ConnectionHandler> {
        Some(self.clone())
    }
}

impl<N: FlashblocksP2PNetworkHandle> ConnectionHandler for FlashblocksP2PProtocol<N> {
    type Connection = FlashblocksConnection<N>;

    fn protocol(&self) -> Protocol {
        Protocol::new(Self::capability(), 5)
    }

    fn on_unsupported_by_peer(
        self,
        _supported: &SharedCapabilities,
        _direction: Direction,
        _peer_id: PeerId,
    ) -> OnNotSupported {
        OnNotSupported::KeepAlive
    }

    fn into_connection(
        self,
        direction: Direction,
        peer_id: PeerId,
        conn: ProtocolConnection,
    ) -> Self::Connection {
        let capability = Self::capability();

        info!(
            target: "flashblocks::p2p",
            %peer_id,
            %direction,
            capability = %capability.name,
            version = %capability.version,
            "new flashblocks connection"
        );

        FlashblocksConnection::new(self, conn, peer_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use enr::{Enr, secp256k1::SecretKey};
    use reth_eth_wire::{Capabilities, EthVersion, Status, StatusMessage, UnifiedStatus};
    use reth_network::{
        PeerInfo, PeersInfo,
        types::{PeerKind, Reputation, ReputationChangeKind},
    };
    use reth_network_api::{NetworkError, noop::NoopNetwork};
    use reth_network_peers::NodeRecord;
    use std::{
        collections::VecDeque,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    #[derive(Clone, Debug, Default)]
    struct MockNetwork {
        noop: NoopNetwork,
        peer_lookup_responses: Arc<Mutex<VecDeque<Option<PeerInfo>>>>,
        lookup_calls: Arc<AtomicUsize>,
        disconnected_peers: Arc<Mutex<Vec<PeerId>>>,
    }

    impl MockNetwork {
        fn with_peer_lookup_responses(peer_lookup_responses: Vec<Option<PeerInfo>>) -> Self {
            Self {
                peer_lookup_responses: Arc::new(Mutex::new(peer_lookup_responses.into())),
                ..Default::default()
            }
        }

        fn lookup_calls(&self) -> usize {
            self.lookup_calls.load(Ordering::SeqCst)
        }

        fn disconnected_peers(&self) -> Vec<PeerId> {
            self.disconnected_peers.lock().clone()
        }
    }

    impl PeersInfo for MockNetwork {
        fn num_connected_peers(&self) -> usize {
            self.noop.num_connected_peers()
        }

        fn local_node_record(&self) -> NodeRecord {
            self.noop.local_node_record()
        }

        fn local_enr(&self) -> Enr<SecretKey> {
            self.noop.local_enr()
        }
    }

    impl Peers for MockNetwork {
        fn add_trusted_peer_id(&self, _peer: PeerId) {}

        fn add_peer_kind(
            &self,
            _peer: PeerId,
            _kind: PeerKind,
            _tcp_addr: SocketAddr,
            _udp_addr: Option<SocketAddr>,
        ) {
        }

        async fn get_peers_by_kind(&self, _kind: PeerKind) -> Result<Vec<PeerInfo>, NetworkError> {
            Ok(vec![])
        }

        async fn get_all_peers(&self) -> Result<Vec<PeerInfo>, NetworkError> {
            Ok(vec![])
        }

        async fn get_peer_by_id(&self, _peer_id: PeerId) -> Result<Option<PeerInfo>, NetworkError> {
            self.lookup_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.peer_lookup_responses.lock().pop_front().flatten())
        }

        async fn get_peers_by_id(
            &self,
            _peer_ids: Vec<PeerId>,
        ) -> Result<Vec<PeerInfo>, NetworkError> {
            Ok(vec![])
        }

        fn remove_peer(&self, _peer: PeerId, _kind: PeerKind) {}

        fn disconnect_peer(&self, peer: PeerId) {
            self.disconnected_peers.lock().push(peer);
        }

        fn disconnect_peer_with_reason(
            &self,
            peer: PeerId,
            _reason: reth_eth_wire::DisconnectReason,
        ) {
            self.disconnect_peer(peer);
        }

        fn connect_peer_kind(
            &self,
            _peer: PeerId,
            _kind: PeerKind,
            _tcp_addr: SocketAddr,
            _udp_addr: Option<SocketAddr>,
        ) {
        }

        fn reputation_change(&self, _peer_id: PeerId, _kind: ReputationChangeKind) {}

        async fn reputation_by_id(
            &self,
            _peer_id: PeerId,
        ) -> Result<Option<Reputation>, NetworkError> {
            Ok(None)
        }
    }

    fn test_fanout_args() -> FanoutArgs {
        FanoutArgs::default()
    }

    fn test_ctx(fanout_args: FanoutArgs) -> FlashblocksP2PCtx {
        let authorizer = SigningKey::from_bytes(&[7; 32]);

        FlashblocksP2PCtx {
            authorizer_vk: authorizer.verifying_key(),
            fanout_args,
            flashblock_tx: broadcast::Sender::new(16),
        }
    }

    fn test_peer_state(trusted: bool) -> FlashblocksConnectionState {
        let mut state = FlashblocksConnectionState::new();
        state.trusted = trusted;
        state
    }

    /// Creates a peer state with a per-peer outbound channel for message assertions.
    fn test_peer_state_with_channel(
        trusted: bool,
    ) -> (
        FlashblocksConnectionState,
        mpsc::UnboundedReceiver<BytesMut>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut state = FlashblocksConnectionState::new();
        state.trusted = trusted;
        state.outbound_tx = Some(tx);
        (state, rx)
    }

    /// Receives and decodes a direct control message from a per-peer channel.
    fn recv_direct(rx: &mut mpsc::UnboundedReceiver<BytesMut>) -> FlashblocksP2PMsg {
        let bytes = rx.try_recv().expect("expected a direct message");
        FlashblocksP2PMsg::decode(&mut &bytes[..]).expect("valid message")
    }

    fn peer_state(fanout: &FlashblocksP2PState, peer_id: PeerId) -> &FlashblocksConnectionState {
        fanout.connection_state(&peer_id).expect("peer exists")
    }

    fn apply_observation(
        fanout: &mut FlashblocksP2PState,
        authorization: &Authorization,
        flashblock: &FlashblocksPayloadV1,
        peer_id: PeerId,
    ) {
        fanout.note_peer_received_flashblock(authorization, flashblock, peer_id);
    }

    fn test_peer_info(peer_id: PeerId, trusted: bool) -> PeerInfo {
        PeerInfo {
            capabilities: Arc::new(Capabilities::new(vec![])),
            remote_id: peer_id,
            client_version: Arc::<str>::from("mock"),
            enode: "enode://mock".to_owned(),
            enr: None,
            remote_addr: SocketAddr::from((IpAddr::V4(Ipv4Addr::LOCALHOST), 30303)),
            local_addr: None,
            direction: Direction::Incoming,
            eth_version: EthVersion::Eth67,
            status: Arc::new(UnifiedStatus::from_message(StatusMessage::Legacy(Status {
                version: EthVersion::Eth67,
                ..Status::default()
            }))),
            session_established: Instant::now(),
            kind: if trusted {
                PeerKind::Trusted
            } else {
                PeerKind::Basic
            },
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn on_peer_connected_retries_until_peer_info_is_available() {
        let authorizer = SigningKey::from_bytes(&[7; 32]);
        let mut fanout_args = test_fanout_args();
        fanout_args.max_receive_peers = 1;
        let handle = FlashblocksHandle::with_fanout_args(
            authorizer.verifying_key(),
            Some(SigningKey::from_bytes(&[8; 32])),
            fanout_args,
        );
        let peer_id = PeerId::random();
        let network = MockNetwork::with_peer_lookup_responses(vec![
            None,
            Some(test_peer_info(peer_id, true)),
        ]);
        let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel();

        handle.on_peer_connected(network.clone(), peer_id, outbound_tx);

        assert!(network.lookup_calls() >= 2);
        assert!(network.disconnected_peers().is_empty());
        let state = handle.state.lock();
        assert!(
            state
                .connection_state(&peer_id)
                .expect("peer exists")
                .trusted
        );
        drop(state);
        assert_eq!(
            recv_direct(&mut outbound_rx),
            FlashblocksP2PMsg::RequestFlashblocks
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn on_peer_connected_defaults_to_untrusted_when_peer_info_never_arrives() {
        let authorizer = SigningKey::from_bytes(&[7; 32]);
        let mut fanout_args = test_fanout_args();
        fanout_args.max_receive_peers = 1;
        let handle = FlashblocksHandle::with_fanout_args(
            authorizer.verifying_key(),
            Some(SigningKey::from_bytes(&[8; 32])),
            fanout_args,
        );
        let peer_id = PeerId::random();
        let network = MockNetwork::default();
        let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel();

        handle.on_peer_connected(network.clone(), peer_id, outbound_tx);

        assert!(network.lookup_calls() > 1);
        assert!(network.disconnected_peers().is_empty());
        let state = handle.state.lock();
        assert!(
            !state
                .connection_state(&peer_id)
                .expect("peer exists")
                .trusted
        );
        drop(state);
        assert_eq!(
            recv_direct(&mut outbound_rx),
            FlashblocksP2PMsg::RequestFlashblocks
        );
    }

    #[test]
    fn publish_sends_flashblocks_only_to_send_enabled_peers() {
        let ctx = test_ctx(test_fanout_args());
        let mut fanout = FlashblocksP2PState::default();
        let authorizer = SigningKey::from_bytes(&[7; 32]);
        let builder = SigningKey::from_bytes(&[9; 32]);
        let payload_id = PayloadId::new([1; 8]);
        let authorization = Authorization::new(payload_id, 1, &authorizer, builder.verifying_key());
        let flashblock = FlashblocksPayloadV1 {
            payload_id,
            index: 0,
            ..Default::default()
        };
        let authorized_payload =
            AuthorizedPayload::new(&builder, authorization, flashblock.clone());
        let expected = FlashblocksP2PMsg::Authorized(authorized_payload.authorized.clone());

        let source_peer = PeerId::random();
        let send_peer = PeerId::random();
        let non_send_peer = PeerId::random();

        let (mut source_state, mut source_rx) = test_peer_state_with_channel(false);
        source_state.send_enabled = true;
        let (mut send_state, mut send_rx) = test_peer_state_with_channel(false);
        send_state.send_enabled = true;
        let (non_send_state, mut non_send_rx) = test_peer_state_with_channel(false);

        fanout.connections.insert(source_peer, source_state);
        fanout.connections.insert(send_peer, send_state);
        fanout.connections.insert(non_send_peer, non_send_state);
        apply_observation(&mut fanout, &authorization, &flashblock, source_peer);

        ctx.publish(&mut fanout, authorized_payload);

        assert_eq!(recv_direct(&mut send_rx), expected);
        assert!(source_rx.try_recv().is_err());
        assert!(non_send_rx.try_recv().is_err());
    }

    #[test]
    fn trusted_peers_are_requested_first() {
        let mut fanout_args = test_fanout_args();
        fanout_args.max_receive_peers = 1;
        let ctx = test_ctx(fanout_args);
        let mut fanout = FlashblocksP2PState::default();

        let trusted_peer = PeerId::random();
        let untrusted_peer = PeerId::random();
        let (trusted_state, mut trusted_rx) = test_peer_state_with_channel(true);
        let untrusted_state = test_peer_state(false);
        fanout.connections.insert(trusted_peer, trusted_state);
        fanout.connections.insert(untrusted_peer, untrusted_state);

        fanout.maybe_request_receive_peers(&ctx);

        assert_eq!(
            peer_state(&fanout, trusted_peer).receive_status,
            ReceiveStatus::Requesting
        );
        assert_eq!(
            peer_state(&fanout, untrusted_peer).receive_status,
            ReceiveStatus::NotReceiving
        );
        assert_eq!(
            recv_direct(&mut trusted_rx),
            FlashblocksP2PMsg::RequestFlashblocks
        );
    }

    #[test]
    fn trusted_request_bypasses_non_trusted_limit() {
        let mut fanout_args = test_fanout_args();
        fanout_args.max_send_peers = 1;
        let ctx = test_ctx(fanout_args);
        let mut fanout = FlashblocksP2PState::default();

        let victim = PeerId::random();
        let trusted_requester = PeerId::random();
        let (mut victim_state, mut victim_rx) = test_peer_state_with_channel(false);
        let (requester_state, mut requester_rx) = test_peer_state_with_channel(true);
        victim_state.send_enabled = true;
        fanout.connections.insert(victim, victim_state);
        fanout
            .connections
            .insert(trusted_requester, requester_state);

        assert!(!fanout.handle_request(&ctx, trusted_requester));

        assert!(peer_state(&fanout, victim).send_enabled);
        assert!(peer_state(&fanout, trusted_requester).send_enabled);
        assert!(victim_rx.try_recv().is_err());
        assert_eq!(
            recv_direct(&mut requester_rx),
            FlashblocksP2PMsg::AcceptFlashblocks
        );
    }

    #[test]
    fn rotation_replaces_peer_before_requesting_candidate() {
        let mut fanout_args = test_fanout_args();
        fanout_args.max_receive_peers = 1;
        fanout_args.score_samples = 4;
        let score_samples = fanout_args.score_samples;
        let ctx = test_ctx(fanout_args);
        let mut fanout = FlashblocksP2PState::default();

        let current_peer = PeerId::random();
        let candidate_peer = PeerId::random();
        let (mut current_state, mut current_rx) = test_peer_state_with_channel(false);
        let (candidate_state, mut candidate_rx) = test_peer_state_with_channel(false);
        let mut score = Score::new(score_samples);
        score.record(42);
        current_state.receive_status = ReceiveStatus::Receiving { score };
        fanout.connections.insert(current_peer, current_state);
        fanout.connections.insert(candidate_peer, candidate_state);

        fanout.maybe_start_rotation(&ctx);

        assert_eq!(
            peer_state(&fanout, current_peer).receive_status,
            ReceiveStatus::NotReceiving
        );
        assert_eq!(
            peer_state(&fanout, candidate_peer).receive_status,
            ReceiveStatus::Requesting
        );

        assert_eq!(
            recv_direct(&mut current_rx),
            FlashblocksP2PMsg::CancelFlashblocks
        );
        assert_eq!(
            recv_direct(&mut candidate_rx),
            FlashblocksP2PMsg::RequestFlashblocks
        );

        assert!(!fanout.handle_accept(&ctx, candidate_peer));

        assert!(matches!(
            peer_state(&fanout, candidate_peer).receive_status,
            ReceiveStatus::Receiving { .. }
        ));
    }

    #[test]
    fn multiple_pending_requests_clear_independently() {
        let mut fanout_args = test_fanout_args();
        fanout_args.max_receive_peers = 2;
        let ctx = test_ctx(fanout_args);
        let mut fanout = FlashblocksP2PState::default();

        let first_peer = PeerId::random();
        let second_peer = PeerId::random();
        let (first_state, mut first_rx) = test_peer_state_with_channel(false);
        let (second_state, mut second_rx) = test_peer_state_with_channel(false);
        fanout.connections.insert(first_peer, first_state);
        fanout.connections.insert(second_peer, second_state);

        fanout.maybe_request_receive_peers(&ctx);

        assert_eq!(
            peer_state(&fanout, first_peer).receive_status,
            ReceiveStatus::Requesting
        );
        assert_eq!(
            peer_state(&fanout, second_peer).receive_status,
            ReceiveStatus::Requesting
        );

        assert_eq!(
            recv_direct(&mut first_rx),
            FlashblocksP2PMsg::RequestFlashblocks
        );
        assert_eq!(
            recv_direct(&mut second_rx),
            FlashblocksP2PMsg::RequestFlashblocks
        );

        assert!(!fanout.handle_accept(&ctx, first_peer));
        assert!(!fanout.handle_accept(&ctx, second_peer));

        assert!(matches!(
            peer_state(&fanout, first_peer).receive_status,
            ReceiveStatus::Receiving { .. }
        ));
        assert!(matches!(
            peer_state(&fanout, second_peer).receive_status,
            ReceiveStatus::Receiving { .. }
        ));
    }

    #[test]
    fn rejected_peer_is_not_immediately_retried() {
        let mut fanout_args = test_fanout_args();
        fanout_args.max_receive_peers = 1;
        let ctx = test_ctx(fanout_args);
        let mut fanout = FlashblocksP2PState::default();

        let peer = PeerId::random();
        let (candidate_state, mut peer_rx) = test_peer_state_with_channel(false);
        fanout.connections.insert(peer, candidate_state);

        fanout.maybe_request_receive_peers(&ctx);
        assert_eq!(
            recv_direct(&mut peer_rx),
            FlashblocksP2PMsg::RequestFlashblocks
        );

        assert!(!fanout.handle_reject(&ctx, peer));

        assert_eq!(
            peer_state(&fanout, peer).receive_status,
            ReceiveStatus::NotReceiving
        );
        assert!(peer_rx.try_recv().is_err());

        fanout.maybe_request_receive_peers(&ctx);
        assert!(peer_rx.try_recv().is_err());

        fanout
            .connection_state_mut(&peer)
            .expect("peer exists")
            .receive_status_timestamp =
            Utc::now().timestamp() as u64 - ctx.fanout_args.rotation_interval.max(1);

        fanout.maybe_request_receive_peers(&ctx);
        assert_eq!(
            recv_direct(&mut peer_rx),
            FlashblocksP2PMsg::RequestFlashblocks
        );
    }

    #[test]
    fn timed_out_request_is_cleared_and_replaced() {
        let mut fanout_args = test_fanout_args();
        fanout_args.max_receive_peers = 1;
        let ctx = test_ctx(fanout_args);
        let mut fanout = FlashblocksP2PState::default();

        let stale_peer = PeerId::random();
        let replacement_peer = PeerId::random();
        let (stale_state, mut stale_rx) = test_peer_state_with_channel(true);
        let (replacement_state, mut replacement_rx) = test_peer_state_with_channel(false);
        fanout.connections.insert(stale_peer, stale_state);
        fanout
            .connections
            .insert(replacement_peer, replacement_state);

        fanout.maybe_request_receive_peers(&ctx);

        assert_eq!(
            peer_state(&fanout, stale_peer).receive_status,
            ReceiveStatus::Requesting
        );
        assert_eq!(
            recv_direct(&mut stale_rx),
            FlashblocksP2PMsg::RequestFlashblocks
        );

        fanout
            .connection_state_mut(&stale_peer)
            .expect("peer exists")
            .receive_status_timestamp =
            Utc::now().timestamp() as u64 - RECEIVE_REQUEST_TIMEOUT_SECS;

        fanout.expire_stale_receive_requests(&ctx);

        assert_eq!(
            peer_state(&fanout, stale_peer).receive_status,
            ReceiveStatus::NotReceiving
        );
        assert_eq!(
            peer_state(&fanout, replacement_peer).receive_status,
            ReceiveStatus::Requesting
        );
        assert_eq!(
            recv_direct(&mut replacement_rx),
            FlashblocksP2PMsg::RequestFlashblocks
        );
        assert!(stale_rx.try_recv().is_err());
    }

    #[test]
    fn silent_receive_peer_can_be_rotated_out_without_samples() {
        let mut fanout_args = test_fanout_args();
        fanout_args.max_receive_peers = 1;
        let ctx = test_ctx(fanout_args);
        let mut fanout = FlashblocksP2PState::default();

        let silent_peer = PeerId::random();
        let replacement_peer = PeerId::random();

        let (mut silent_state, mut silent_rx) = test_peer_state_with_channel(false);
        let (replacement_state, mut replacement_rx) = test_peer_state_with_channel(true);

        silent_state.receive_status = ReceiveStatus::Receiving {
            score: Score::new(4),
        };

        fanout.connections.insert(silent_peer, silent_state);
        fanout
            .connections
            .insert(replacement_peer, replacement_state);

        fanout.maybe_start_rotation(&ctx);

        assert_eq!(
            peer_state(&fanout, silent_peer).receive_status,
            ReceiveStatus::NotReceiving
        );
        assert_eq!(
            peer_state(&fanout, replacement_peer).receive_status,
            ReceiveStatus::Requesting
        );

        assert_eq!(
            recv_direct(&mut silent_rx),
            FlashblocksP2PMsg::CancelFlashblocks
        );
        assert_eq!(
            recv_direct(&mut replacement_rx),
            FlashblocksP2PMsg::RequestFlashblocks
        );
    }

    #[test]
    fn peer_score_penalizes_missed_flashblocks() {
        let mut fanout_args = test_fanout_args();
        fanout_args.max_receive_peers = 2;
        fanout_args.score_samples = 4;
        let score_samples = fanout_args.score_samples;
        let mut fanout = FlashblocksP2PState::default();

        let steady_peer = PeerId::random();
        let lagging_peer = PeerId::random();
        let mut steady_state = test_peer_state(false);
        let mut lagging_state = test_peer_state(false);
        let authorizer = SigningKey::from_bytes(&[7; 32]);
        let builder = SigningKey::from_bytes(&[9; 32]);

        let mut steady_score = Score::new(score_samples);
        steady_score.record(10);
        steady_state.receive_status = ReceiveStatus::Receiving {
            score: steady_score,
        };
        let mut lagging_score = Score::new(score_samples);
        lagging_score.record(100);
        lagging_state.receive_status = ReceiveStatus::Receiving {
            score: lagging_score,
        };

        fanout.connections.insert(steady_peer, steady_state);
        fanout.connections.insert(lagging_peer, lagging_state);

        for index in 0..=RECEIVE_FLASHBLOCK_GRACE_WINDOW {
            let authorization = Authorization::new(
                PayloadId::default(),
                index as u64,
                &authorizer,
                builder.verifying_key(),
            );
            let flashblock = FlashblocksPayloadV1 {
                payload_id: PayloadId::default(),
                index: index as u64,
                ..Default::default()
            };
            apply_observation(&mut fanout, &authorization, &flashblock, steady_peer);
        }

        assert_eq!(fanout.worst_receive_peer(), Some(lagging_peer));
        let ReceiveStatus::Receiving {
            score: steady_score,
        } = &peer_state(&fanout, steady_peer).receive_status
        else {
            panic!("expected Receiving");
        };
        assert_eq!(steady_score.value(), Some(10));
        let ReceiveStatus::Receiving {
            score: lagging_score,
        } = &peer_state(&fanout, lagging_peer).receive_status
        else {
            panic!("expected Receiving");
        };
        assert_eq!(
            lagging_score.value(),
            Some((100 * (score_samples - 1) + MISSED_FLASHBLOCK_PENALTY_NS) / score_samples)
        );
    }

    #[test]
    fn pending_candidate_is_rotated_out_after_missing_blocks() {
        let mut fanout_args = test_fanout_args();
        fanout_args.max_receive_peers = 2;
        fanout_args.score_samples = 4;
        let score_samples = fanout_args.score_samples;
        let ctx = test_ctx(fanout_args);
        let mut fanout = FlashblocksP2PState::default();

        let steady_peer = PeerId::random();
        let rotating_peer = PeerId::random();
        let candidate_peer = PeerId::random();
        let replacement_peer = PeerId::random();

        let mut steady_state = test_peer_state(false);
        let mut rotating_state = test_peer_state(false);
        let candidate_state = test_peer_state(true);
        let replacement_state = test_peer_state(true);

        let mut steady_score = Score::new(score_samples);
        steady_score.record(10);
        steady_state.receive_status = ReceiveStatus::Receiving {
            score: steady_score,
        };
        let mut rotating_score = Score::new(score_samples);
        rotating_score.record(100);
        rotating_state.receive_status = ReceiveStatus::Receiving {
            score: rotating_score,
        };

        fanout.connections.insert(steady_peer, steady_state);
        fanout.connections.insert(rotating_peer, rotating_state);
        fanout.connections.insert(candidate_peer, candidate_state);

        fanout.maybe_start_rotation(&ctx);

        // Accept the candidate so it transitions to Receiving.
        assert!(!fanout.handle_accept(&ctx, candidate_peer));

        let authorizer = SigningKey::from_bytes(&[7; 32]);
        let builder = SigningKey::from_bytes(&[9; 32]);
        for index in 0..=RECEIVE_FLASHBLOCK_GRACE_WINDOW {
            let authorization = Authorization::new(
                PayloadId::default(),
                index as u64,
                &authorizer,
                builder.verifying_key(),
            );
            let flashblock = FlashblocksPayloadV1 {
                payload_id: PayloadId::default(),
                index: index as u64,
                ..Default::default()
            };
            apply_observation(&mut fanout, &authorization, &flashblock, steady_peer);
        }

        assert_eq!(fanout.worst_receive_peer(), Some(candidate_peer));

        fanout
            .connections
            .insert(replacement_peer, replacement_state);
        fanout.maybe_start_rotation(&ctx);

        assert_eq!(
            peer_state(&fanout, candidate_peer).receive_status,
            ReceiveStatus::NotReceiving
        );
        assert_eq!(
            peer_state(&fanout, replacement_peer).receive_status,
            ReceiveStatus::Requesting
        );
    }

    #[test]
    fn unsolicited_accept_is_penalized() {
        let ctx = test_ctx(test_fanout_args());
        let mut fanout = FlashblocksP2PState::default();

        let peer = PeerId::random();
        let state = test_peer_state(false);
        fanout.connections.insert(peer, state);

        // Accept without a prior request should be penalized.
        assert!(fanout.handle_accept(&ctx, peer));
    }

    #[test]
    fn unsolicited_reject_is_penalized() {
        let ctx = test_ctx(test_fanout_args());
        let mut fanout = FlashblocksP2PState::default();

        let peer = PeerId::random();
        let state = test_peer_state(false);
        fanout.connections.insert(peer, state);

        // Reject without a prior request should be penalized.
        assert!(fanout.handle_reject(&ctx, peer));
    }

    #[test]
    fn cancel_without_relationship_is_penalized() {
        let ctx = test_ctx(test_fanout_args());
        let mut fanout = FlashblocksP2PState::default();

        let peer = PeerId::random();
        let state = test_peer_state(false);
        fanout.connections.insert(peer, state);

        // Cancel with no send/receive relationship should be penalized.
        assert!(fanout.handle_cancel(&ctx, peer));
    }

    #[test]
    fn cancel_only_clears_send_direction() {
        let ctx = test_ctx(test_fanout_args());
        let mut fanout = FlashblocksP2PState::default();

        let peer = PeerId::random();
        let mut state = test_peer_state(false);
        state.send_enabled = true;
        state.receive_status = ReceiveStatus::Receiving {
            score: Score::new(4),
        };
        fanout.connections.insert(peer, state);

        assert!(!fanout.handle_cancel(&ctx, peer));
        assert!(!peer_state(&fanout, peer).send_enabled);
        assert!(matches!(
            peer_state(&fanout, peer).receive_status,
            ReceiveStatus::Receiving { .. }
        ));
    }

    #[test]
    fn cancel_from_sender_is_penalized() {
        let ctx = test_ctx(test_fanout_args());
        let mut fanout = FlashblocksP2PState::default();

        let peer = PeerId::random();
        let mut state = test_peer_state(false);
        state.receive_status = ReceiveStatus::Receiving {
            score: Score::new(4),
        };
        fanout.connections.insert(peer, state);

        assert!(fanout.handle_cancel(&ctx, peer));
    }

    #[test]
    fn duplicate_request_when_already_sending_is_penalized() {
        let ctx = test_ctx(test_fanout_args());
        let mut fanout = FlashblocksP2PState::default();

        let peer = PeerId::random();
        let mut state = test_peer_state(false);
        state.send_enabled = true;
        fanout.connections.insert(peer, state);

        assert!(fanout.handle_request(&ctx, peer));
    }

    #[test]
    fn receive_retry_cooldown_does_not_penalize_inbound_request() {
        let ctx = test_ctx(test_fanout_args());
        let mut fanout = FlashblocksP2PState::default();

        let peer = PeerId::random();
        let (mut state, mut rx) = test_peer_state_with_channel(false);
        state.receive_status_timestamp = Utc::now().timestamp() as u64;
        fanout.connections.insert(peer, state);

        assert!(!fanout.handle_request(&ctx, peer));
        assert!(peer_state(&fanout, peer).send_enabled);
        assert_eq!(recv_direct(&mut rx), FlashblocksP2PMsg::AcceptFlashblocks);
    }

    #[test]
    fn repeated_rejected_requests_are_rate_limited() {
        let mut fanout_args = test_fanout_args();
        fanout_args.max_send_peers = 0;
        let ctx = test_ctx(fanout_args);
        let mut fanout = FlashblocksP2PState::default();

        let peer = PeerId::random();
        let (state, mut rx) = test_peer_state_with_channel(false);
        fanout.connections.insert(peer, state);

        for _ in 0..MAX_CONTROL_MSGS_PER_WINDOW {
            assert!(!fanout.handle_request(&ctx, peer));
            assert_eq!(recv_direct(&mut rx), FlashblocksP2PMsg::RejectFlashblocks);
        }
        assert!(fanout.handle_request(&ctx, peer));
    }

    #[test]
    fn control_message_rate_limit_triggers_penalty() {
        let ctx = test_ctx(test_fanout_args());
        let mut fanout = FlashblocksP2PState::default();

        let peer = PeerId::random();
        let mut state = test_peer_state(false);
        state.send_enabled = true;
        fanout.connections.insert(peer, state);

        // Spam requests to exceed the rate limit.
        for _ in 0..MAX_CONTROL_MSGS_PER_WINDOW {
            // These return true because send_enabled is already set (duplicate request),
            // but the rate limit hasn't been hit yet.
            assert!(fanout.handle_request(&ctx, peer));
        }
        // The next one should hit the rate limit.
        assert!(fanout.handle_request(&ctx, peer));
    }
}
