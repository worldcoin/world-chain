use crate::protocol::{
    connection::{FlashblocksConnection, FlashblocksConnectionState, Score},
    error::FlashblocksP2PError,
};
use alloy_rlp::BytesMut;
use chrono::Utc;
use ed25519_dalek::{SigningKey, VerifyingKey};
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
    time::Duration,
};
use tokio::{
    sync::{broadcast, watch},
    time,
};
use tracing::{debug, info, warn};

use reth_ethereum::network::{
    api::Direction,
    eth_wire::{capability::SharedCapabilities, multiplex::ProtocolConnection, protocol::Protocol},
    protocol::{ConnectionHandler, OnNotSupported},
};
use tokio_stream::wrappers::BroadcastStream;

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
/// This must be at least long enough to cover the max authorization age to prevent a spam
/// attack.
pub(crate) const RECEIVE_FLASHBLOCK_GRACE_WINDOW: usize = 50;

/// Trait bound for network handles that can be used with the flashblocks P2P protocol.
///
/// This trait combines all the necessary bounds for a network handle to be used
/// in the flashblocks P2P system, including peer management capabilities.
pub trait FlashblocksP2PNetworkHandle: Clone + Unpin + Peers + std::fmt::Debug + 'static {}

impl<N: Clone + Unpin + Peers + std::fmt::Debug + 'static> FlashblocksP2PNetworkHandle for N {}

/// Messages that can be broadcast over a channel to each internal peer connection.
///
/// These messages are used internally to coordinate the broadcasting of flashblocks
/// and publishing status changes to all connected peers.
#[derive(Clone, Debug)]
pub enum PeerMsg {
    /// Send an already serialized flashblock to all peers.
    FlashblocksPayloadV1((PayloadId, usize, BytesMut)),
    /// Send a previously serialized StartPublish message to all peers.
    StartPublishing(BytesMut),
    /// Send a previously serialized StopPublish message to all peers.
    StopPublishing(BytesMut),
    /// Send an already serialized control message to a single peer.
    Direct { peer_id: PeerId, bytes: BytesMut },
}

#[derive(Clone, Debug)]
pub struct ObservedPayload {
    payload_id: PayloadId,
    timestamp: u64,
    flashblock_index: u64,
    received_peers: HashSet<PeerId>,
}

/// Runtime configuration for bounded flashblocks fanout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FanoutConfig {
    /// Maximum number of non-trusted peers to send flashblocks to.
    pub max_send_peers: usize,
    /// Maximum number of peers to receive flashblocks from.
    pub max_receive_peers: usize,
    /// How often to evaluate latency-based peer rotation.
    pub rotation_interval: Duration,
    /// How long to wait for request flashblocks to be answered.
    pub request_flashblocks_timeout: Duration,
    /// Number of latency measurements to retain per receive peer.
    pub latency_window: i64,
}

impl Default for FanoutConfig {
    fn default() -> Self {
        Self {
            max_send_peers: 10,
            max_receive_peers: 3,
            rotation_interval: Duration::from_secs(30),
            request_flashblocks_timeout: Duration::from_secs(2),
            latency_window: 1000,
        }
    }
}

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
    /// All currently connected peers and their shared connection state.
    pub connections: HashMap<PeerId, Arc<Mutex<FlashblocksConnectionState>>>,
    /// The peer currently occupying the outstanding request slot, if any.
    pub awaiting_flashblocks_req: Option<PeerId>,
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
            awaiting_flashblocks_req: None,
        }
    }
}

impl FlashblocksP2PState {
    /// Returns the current publishing status of this node.
    ///
    /// This indicates whether the node is actively publishing flashblocks,
    /// waiting to publish, or not publishing at all.
    pub fn publishing_status(&self) -> PublishingStatus {
        self.publishing_status.borrow().clone()
    }

    /// Returns the connection state of a peer.
    fn connection_state(
        &self,
        peer_id: &PeerId,
    ) -> Option<&Arc<Mutex<FlashblocksConnectionState>>> {
        self.connections.get(peer_id)
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
            for (peer_id, connection) in self.connections.iter() {
                let mut connection = connection.lock();
                if connection.receive_enabled_timestamp < evicted.timestamp + 2
                    && !evicted.received_peers.contains(peer_id)
                {
                    if let Some(score) = connection.receive_enabled.as_mut() {
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
        }

        self.observed_payloads.push_back(ObservedPayload {
            payload_id: flashblock.payload_id,
            timestamp: authorization.timestamp,
            flashblock_index: flashblock.index,
            received_peers: HashSet::from([peer_id]),
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

    fn num_receive_peers(&self) -> usize {
        self.connections
            .iter()
            .filter(|(_, peer_state)| peer_state.lock().receive_enabled.is_some())
            .count()
    }

    fn available_receive_candidates(&self) -> Vec<(PeerId, bool)> {
        self.connections
            .iter()
            .filter_map(|(peer_id, peer_state)| {
                let peer_state = peer_state.lock();
                if peer_state.trusted_known
                    && peer_state.receive_enabled.is_none()
                    && !peer_state.request_in_flight
                {
                    Some((*peer_id, peer_state.trusted))
                } else {
                    None
                }
            })
            .collect()
    }

    fn begin_requesting_peer(&mut self, ctx: &FlashblocksP2PCtx, peer_id: PeerId) {
        let Some(peer_state) = self.connection_state(&peer_id) else {
            return;
        };
        let timestamp = Utc::now().timestamp() as u64;
        let mut peer_state = peer_state.lock();
        peer_state.request_in_flight = true;
        peer_state.receive_enabled = Some(Score::new(ctx.fanout_config.latency_window));
        peer_state.receive_enabled_timestamp = timestamp;
        drop(peer_state);
        self.awaiting_flashblocks_req = Some(peer_id);
        ctx.send_direct(peer_id, FlashblocksP2PMsg::RequestFlashblocks);
    }

    pub fn maybe_request_receive_peers(&mut self, ctx: &FlashblocksP2PCtx) {
        if self.num_receive_peers() >= ctx.fanout_config.max_receive_peers {
            return;
        }

        let candidates = self.available_receive_candidates();
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

    fn worst_receive_peer(&self) -> Option<(PeerId, i64)> {
        self.connections
            .iter()
            .filter_map(|(peer_id, peer_state)| {
                let peer_state = peer_state.lock();
                peer_state
                    .receive_enabled
                    .as_ref()
                    .and_then(|score| score.value().map(|score| (*peer_id, score)))
            })
            .max_by(|(_, lhs), (_, rhs)| lhs.cmp(rhs))
    }

    fn maybe_start_rotation(&mut self, ctx: &FlashblocksP2PCtx) {
        if self.num_receive_peers() < ctx.fanout_config.max_receive_peers {
            return;
        }

        let Some((evict, _)) = self.worst_receive_peer() else {
            return;
        };

        let mut candidates = self.available_receive_candidates();
        if candidates.is_empty() {
            return;
        }

        let mut rng = rand::rng();
        candidates.shuffle(&mut rng);
        let candidate = candidates
            .iter()
            .find_map(|(peer_id, trusted)| (*trusted).then_some(*peer_id))
            .unwrap_or(candidates[0].0);

        if let Some(evict_state) = self.connection_state(&evict) {
            let timestamp = Utc::now().timestamp() as u64;
            let mut evict_state = evict_state.lock();
            evict_state.receive_enabled = None;
            evict_state.request_in_flight = false;
            evict_state.receive_enabled_timestamp = timestamp;
        }
        if self.awaiting_flashblocks_req == Some(evict) {
            self.awaiting_flashblocks_req = None;
        }
        ctx.send_direct(evict, FlashblocksP2PMsg::CancelFlashblocks);

        self.begin_requesting_peer(ctx, candidate);
    }

    fn handle_request(&mut self, ctx: &FlashblocksP2PCtx, peer_id: PeerId) {
        let Some(peer_state) = self.connection_state(&peer_id) else {
            return;
        };

        if peer_state.lock().send_enabled {
            ctx.send_direct(peer_id, FlashblocksP2PMsg::AcceptFlashblocks);
            return;
        }

        let peer_is_trusted = peer_state.lock().trusted;
        if peer_is_trusted {
            let non_trusted_send_count = self
                .connections
                .values()
                .filter(|candidate_state| {
                    let candidate_state = candidate_state.lock();
                    candidate_state.send_enabled && !candidate_state.trusted
                })
                .count();
            if non_trusted_send_count >= ctx.fanout_config.max_send_peers {
                if let Some(evicted_peer) =
                    self.connections
                        .iter()
                        .find_map(|(candidate, candidate_state)| {
                            let candidate_state = candidate_state.lock();
                            (candidate_state.send_enabled && !candidate_state.trusted)
                                .then_some(*candidate)
                        })
                {
                    if let Some(evicted_state) = self.connection_state(&evicted_peer) {
                        evicted_state.lock().send_enabled = false;
                    }
                    ctx.send_direct(evicted_peer, FlashblocksP2PMsg::CancelFlashblocks);
                }
            }

            peer_state.lock().send_enabled = true;
            ctx.send_direct(peer_id, FlashblocksP2PMsg::AcceptFlashblocks);
            return;
        }

        let non_trusted_send_count = self
            .connections
            .values()
            .filter(|candidate_state| {
                let candidate_state = candidate_state.lock();
                candidate_state.send_enabled && !candidate_state.trusted
            })
            .count();
        if non_trusted_send_count < ctx.fanout_config.max_send_peers {
            peer_state.lock().send_enabled = true;
            ctx.send_direct(peer_id, FlashblocksP2PMsg::AcceptFlashblocks);
        } else {
            ctx.send_direct(peer_id, FlashblocksP2PMsg::RejectFlashblocks);
        }
    }

    fn handle_accept(&mut self, _ctx: &FlashblocksP2PCtx, peer_id: PeerId) {
        let Some(peer_state) = self.connection_state(&peer_id) else {
            return;
        };

        if self.awaiting_flashblocks_req != Some(peer_id) {
            return;
        }

        peer_state.lock().request_in_flight = false;
        self.awaiting_flashblocks_req = None;
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
    /// Builder's signing key used to sign outgoing authorized P2P messages.
    pub builder_sk: Option<SigningKey>,
    /// Fanout configuration for peer selection and rotation.
    pub fanout_config: FanoutConfig,
    /// Broadcast sender for peer messages that will be sent to all connected peers.
    /// Messages may not be strictly ordered due to network conditions.
    pub peer_tx: broadcast::Sender<PeerMsg>,
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
    /// Thread-safe mutable state of the flashblocks protocol.
    /// Protected by a mutex to allow concurrent access from multiple connections.
    pub state: Arc<Mutex<FlashblocksP2PState>>,
}

impl FlashblocksHandle {
    pub fn new(authorizer_vk: VerifyingKey, builder_sk: Option<SigningKey>) -> Self {
        Self::with_fanout_config(authorizer_vk, builder_sk, FanoutConfig::default())
    }

    pub fn with_fanout_config(
        authorizer_vk: VerifyingKey,
        builder_sk: Option<SigningKey>,
        fanout_config: FanoutConfig,
    ) -> Self {
        let flashblock_tx = broadcast::Sender::new(BROADCAST_BUFFER_CAPACITY);
        let peer_tx = broadcast::Sender::new(BROADCAST_BUFFER_CAPACITY);
        let state = Arc::new(Mutex::new(FlashblocksP2PState::default()));
        let ctx = FlashblocksP2PCtx {
            authorizer_vk,
            builder_sk,
            fanout_config,
            peer_tx,
            flashblock_tx,
        };
        let handle = Self { ctx, state };
        let moved_handle = handle.clone();

        tokio::spawn(async move {
            let mut rotation_interval =
                time::interval(moved_handle.ctx.fanout_config.rotation_interval);
            rotation_interval.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
            rotation_interval.tick().await;

            loop {
                rotation_interval.tick().await;
                moved_handle
                    .state
                    .lock()
                    .maybe_start_rotation(&moved_handle.ctx)
            }
        });

        handle
    }

    pub(crate) fn on_peer_connected<N: FlashblocksP2PNetworkHandle>(
        &self,
        network: N,
        peer_id: PeerId,
        fanout_state: Arc<Mutex<FlashblocksConnectionState>>,
    ) {
        {
            let mut state = self.state.lock();
            state.connections.insert(peer_id, fanout_state.clone());
            state.maybe_request_receive_peers(&self.ctx);
        }

        let handle = self.clone();
        tokio::spawn(async move {
            match network.get_peer_by_id(peer_id).await {
                Ok(Some(peer_info)) => {
                    let mut fanout_state_guard = fanout_state.lock();
                    fanout_state_guard.trusted = peer_info.kind.is_trusted();
                    fanout_state_guard.trusted_known = true;
                    drop(fanout_state_guard);

                    let mut state = handle.state.lock();
                    if state
                        .connection_state(&peer_id)
                        .is_some_and(|current| Arc::ptr_eq(&current, &fanout_state))
                    {
                        state.maybe_request_receive_peers(&handle.ctx);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    warn!(
                        target: "flashblocks::p2p",
                        %peer_id,
                        %error,
                        "failed to load peer info for flashblocks fanout",
                    );
                }
            }
        });
    }

    pub(crate) fn on_peer_disconnected(&self, peer_id: PeerId) {
        let mut state = self.state.lock();
        state.connections.remove(&peer_id);
        if state.awaiting_flashblocks_req == Some(peer_id) {
            state.awaiting_flashblocks_req = None;
        }
        state.maybe_request_receive_peers(&self.ctx);
    }

    pub(crate) fn handle_request_message(&self, peer_id: PeerId) {
        let mut state = self.state.lock();
        state.handle_request(&self.ctx, peer_id);
    }

    pub(crate) fn handle_accept_message(&self, peer_id: PeerId) {
        let mut state = self.state.lock();
        state.handle_accept(&self.ctx, peer_id);
    }

    pub(crate) fn handle_reject_message(&self, peer_id: PeerId) {
        let mut state = self.state.lock();
        let this = &mut state;
        let ctx: &FlashblocksP2PCtx = &self.ctx;
        let Some(peer_state) = this.connection_state(&peer_id) else {
            return;
        };

        if this.awaiting_flashblocks_req != Some(peer_id) {
            return;
        }
        {
            let mut peer_state = peer_state.lock();
            peer_state.request_in_flight = false;
            peer_state.receive_enabled = None;
        }
        this.awaiting_flashblocks_req = None;

        this.maybe_request_receive_peers(ctx);
    }

    pub(crate) fn handle_cancel_message(&self, peer_id: PeerId) {
        let mut state = self.state.lock();
        let this = &mut state;
        let mut should_refill = false;
        if let Some(peer_state) = this.connection_state(&peer_id).cloned() {
            let mut peer_state = peer_state.lock();
            peer_state.send_enabled = false;
            if peer_state.receive_enabled.is_some() || peer_state.request_in_flight {
                peer_state.receive_enabled = None;
                peer_state.request_in_flight = false;
                if this.awaiting_flashblocks_req == Some(peer_id) {
                    this.awaiting_flashblocks_req = None;
                }
                should_refill = true;
            }
        }
        if should_refill {
            this.maybe_request_receive_peers(&self.ctx);
        }
    }
}

impl FlashblocksP2PCtx {
    pub(crate) fn send_direct(&self, peer_id: PeerId, msg: FlashblocksP2PMsg) {
        self.peer_tx
            .send(PeerMsg::Direct {
                peer_id,
                bytes: msg.encode(),
            })
            .ok();
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
    /// Returns the builder signing key if configured.
    pub fn builder_sk(&self) -> Result<&SigningKey, FlashblocksP2PError> {
        self.ctx
            .builder_sk
            .as_ref()
            .ok_or(FlashblocksP2PError::MissingBuilderSk)
    }

    /// Publishes a newly created flashblock from the payload builder to the P2P network.
    ///
    /// This method validates that the builder has authorization to publish and that
    /// the authorization matches the current publishing session. The flashblock is
    /// then processed, cached, and broadcast to all connected peers.
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
        self.ctx.publish(&mut state, authorized_payload, None);
        Ok(())
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
                    let peer_msg = PeerMsg::StartPublishing(p2p_msg.encode());
                    self.ctx.peer_tx.send(peer_msg).ok();

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
                    let peer_msg = PeerMsg::StopPublishing(p2p_msg.encode());
                    self.ctx.peer_tx.send(peer_msg).ok();
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
                    let peer_msg = PeerMsg::StopPublishing(p2p_msg.encode());
                    self.ctx.peer_tx.send(peer_msg).ok();
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
    /// The stream will continue to yield flashblocks for consecutive payloads as well, so
    /// consumers should take care to handle the stream appropriately.
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
    /// - Broadcasts to peers and publishes ordered flashblocks to the stream
    pub fn publish(
        &self,
        state: &mut FlashblocksP2PState,
        authorized_payload: AuthorizedPayload<FlashblocksPayloadV1>,
        _source_peer_id: Option<PeerId>,
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

            let peer_msg =
                PeerMsg::FlashblocksPayloadV1((payload.payload_id, payload.index as usize, bytes));

            self.peer_tx.send(peer_msg).ok();

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
        Protocol::new(Self::capability(), 6)
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

        let peer_rx = self.handle.ctx.peer_tx.subscribe();
        let fanout_state = Arc::new(Mutex::new(FlashblocksConnectionState::new()));

        FlashblocksConnection::new(
            self,
            conn,
            peer_id,
            BroadcastStream::new(peer_rx),
            fanout_state,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn test_ctx(config: FanoutConfig) -> FlashblocksP2PCtx {
        let authorizer = SigningKey::from_bytes(&[7; 32]);

        FlashblocksP2PCtx {
            authorizer_vk: authorizer.verifying_key(),
            builder_sk: Some(SigningKey::from_bytes(&[8; 32])),
            fanout_config: config,
            peer_tx: broadcast::Sender::new(16),
            flashblock_tx: broadcast::Sender::new(16),
        }
    }

    fn test_peer_state(
        trusted: bool,
        trusted_known: bool,
    ) -> Arc<Mutex<FlashblocksConnectionState>> {
        let state = Arc::new(Mutex::new(FlashblocksConnectionState::new()));
        {
            let mut state_guard = state.lock();
            state_guard.trusted = trusted;
            state_guard.trusted_known = trusted_known;
        }
        state
    }

    #[test]
    fn trusted_peers_are_requested_first() {
        let config = FanoutConfig {
            max_receive_peers: 1,
            ..Default::default()
        };
        let ctx = test_ctx(config);
        let mut fanout = FlashblocksP2PState::default();
        let mut rx = ctx.peer_tx.subscribe();

        let trusted_peer = PeerId::random();
        let untrusted_peer = PeerId::random();
        let trusted_state = test_peer_state(true, true);
        let untrusted_state = test_peer_state(false, true);
        fanout
            .connections
            .insert(trusted_peer, trusted_state.clone());
        fanout
            .connections
            .insert(untrusted_peer, untrusted_state.clone());

        fanout.maybe_request_receive_peers(&ctx);

        assert!(matches!(
            fanout.awaiting_flashblocks_req,
            Some(peer_id) if peer_id == trusted_peer
        ));
        assert!(trusted_state.lock().request_in_flight);
        assert!(trusted_state.lock().receive_enabled.is_some());
        assert!(!untrusted_state.lock().request_in_flight);
        match rx.try_recv().expect("request sent") {
            PeerMsg::Direct { peer_id, bytes } => {
                assert_eq!(peer_id, trusted_peer);
                assert_eq!(
                    FlashblocksP2PMsg::decode(&mut &bytes[..]).unwrap(),
                    FlashblocksP2PMsg::RequestFlashblocks
                );
            }
            other => panic!("unexpected peer message: {other:?}"),
        }
    }

    #[test]
    fn unknown_peers_are_not_requested_until_trust_is_known() {
        let config = FanoutConfig {
            max_receive_peers: 1,
            ..Default::default()
        };
        let ctx = test_ctx(config);
        let mut fanout = FlashblocksP2PState::default();
        let mut rx = ctx.peer_tx.subscribe();

        let trusted_peer = PeerId::random();
        let untrusted_peer = PeerId::random();
        let trusted_state = test_peer_state(true, false);
        let untrusted_state = test_peer_state(false, false);
        fanout
            .connections
            .insert(trusted_peer, trusted_state.clone());
        fanout
            .connections
            .insert(untrusted_peer, untrusted_state.clone());

        fanout.maybe_request_receive_peers(&ctx);

        assert!(fanout.awaiting_flashblocks_req.is_none());
        assert!(!trusted_state.lock().request_in_flight);
        assert!(!untrusted_state.lock().request_in_flight);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn trusted_request_evicts_non_trusted_sender() {
        let config = FanoutConfig {
            max_send_peers: 1,
            ..Default::default()
        };
        let ctx = test_ctx(config);
        let mut fanout = FlashblocksP2PState::default();
        let mut rx = ctx.peer_tx.subscribe();

        let victim = PeerId::random();
        let trusted_requester = PeerId::random();
        let victim_state = test_peer_state(false, true);
        let requester_state = test_peer_state(true, true);
        victim_state.lock().send_enabled = true;
        fanout.connections.insert(victim, victim_state.clone());
        fanout
            .connections
            .insert(trusted_requester, requester_state.clone());

        fanout.handle_request(&ctx, trusted_requester);

        assert!(!victim_state.lock().send_enabled);
        assert!(requester_state.lock().send_enabled);

        match rx.try_recv().expect("cancel sent") {
            PeerMsg::Direct { peer_id, bytes } => {
                assert_eq!(peer_id, victim);
                assert_eq!(
                    FlashblocksP2PMsg::decode(&mut &bytes[..]).unwrap(),
                    FlashblocksP2PMsg::CancelFlashblocks
                );
            }
            other => panic!("unexpected peer message: {other:?}"),
        }

        match rx.try_recv().expect("accept sent") {
            PeerMsg::Direct { peer_id, bytes } => {
                assert_eq!(peer_id, trusted_requester);
                assert_eq!(
                    FlashblocksP2PMsg::decode(&mut &bytes[..]).unwrap(),
                    FlashblocksP2PMsg::AcceptFlashblocks
                );
            }
            other => panic!("unexpected peer message: {other:?}"),
        }
    }

    #[test]
    fn rotation_replaces_peer_before_requesting_candidate() {
        let config = FanoutConfig {
            max_receive_peers: 1,
            latency_window: 4,
            ..Default::default()
        };
        let latency_window = config.latency_window;
        let ctx = test_ctx(config);
        let mut fanout = FlashblocksP2PState::default();
        let mut rx = ctx.peer_tx.subscribe();

        let current_peer = PeerId::random();
        let candidate_peer = PeerId::random();
        let current_state = test_peer_state(false, true);
        let candidate_state = test_peer_state(false, true);
        let mut score = Score::new(latency_window);
        score.record(42);
        current_state.lock().receive_enabled = Some(score);
        fanout
            .connections
            .insert(current_peer, current_state.clone());
        fanout
            .connections
            .insert(candidate_peer, candidate_state.clone());

        fanout.maybe_start_rotation(&ctx);

        assert!(current_state.lock().receive_enabled.is_none());
        assert!(candidate_state.lock().request_in_flight);
        assert!(candidate_state.lock().receive_enabled.is_some());
        assert!(matches!(
            fanout.awaiting_flashblocks_req,
            Some(candidate) if candidate == candidate_peer
        ));

        match rx.try_recv().expect("cancel sent to old peer") {
            PeerMsg::Direct { peer_id, bytes } => {
                assert_eq!(peer_id, current_peer);
                assert_eq!(
                    FlashblocksP2PMsg::decode(&mut &bytes[..]).unwrap(),
                    FlashblocksP2PMsg::CancelFlashblocks
                );
            }
            other => panic!("unexpected peer message: {other:?}"),
        }

        match rx.try_recv().expect("rotation request sent") {
            PeerMsg::Direct { peer_id, bytes } => {
                assert_eq!(peer_id, candidate_peer);
                assert_eq!(
                    FlashblocksP2PMsg::decode(&mut &bytes[..]).unwrap(),
                    FlashblocksP2PMsg::RequestFlashblocks
                );
            }
            other => panic!("unexpected peer message: {other:?}"),
        }

        fanout.handle_accept(&ctx, candidate_peer);

        assert!(!candidate_state.lock().request_in_flight);
        assert!(candidate_state.lock().receive_enabled.is_some());
        assert!(fanout.awaiting_flashblocks_req.is_none());
    }

    #[test]
    fn peer_score_penalizes_missed_flashblocks() {
        let config = FanoutConfig {
            max_receive_peers: 2,
            latency_window: 4,
            ..Default::default()
        };
        let latency_window = config.latency_window;
        let mut fanout = FlashblocksP2PState::default();

        let steady_peer = PeerId::random();
        let lagging_peer = PeerId::random();
        let steady_state = test_peer_state(false, true);
        let lagging_state = test_peer_state(false, true);
        let authorizer = SigningKey::from_bytes(&[7; 32]);
        let builder = SigningKey::from_bytes(&[9; 32]);

        steady_state.lock().receive_enabled = Some(Score::new(latency_window));
        lagging_state.lock().receive_enabled = Some(Score::new(latency_window));
        steady_state
            .lock()
            .receive_enabled
            .as_mut()
            .expect("steady peer score")
            .record(10);
        lagging_state
            .lock()
            .receive_enabled
            .as_mut()
            .expect("lagging peer score")
            .record(100);

        fanout.connections.insert(steady_peer, steady_state.clone());
        fanout
            .connections
            .insert(lagging_peer, lagging_state.clone());

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
            fanout.note_peer_received_flashblock(&authorization, &flashblock, steady_peer);
        }

        assert_eq!(fanout.worst_receive_peer().map(|(peer_id, _)| peer_id), Some(lagging_peer));
        assert_eq!(
            steady_state
                .lock()
                .receive_enabled
                .as_ref()
                .and_then(Score::value),
            Some(10)
        );
        assert_eq!(
            lagging_state
                .lock()
                .receive_enabled
                .as_ref()
                .and_then(Score::value),
            Some((100 * (latency_window - 1) + MISSED_FLASHBLOCK_PENALTY_NS) / latency_window)
        );
    }

    #[test]
    fn pending_candidate_is_rotated_out_after_missing_blocks() {
        let config = FanoutConfig {
            max_receive_peers: 2,
            latency_window: 4,
            ..Default::default()
        };
        let latency_window = config.latency_window;
        let ctx = test_ctx(config);
        let mut fanout = FlashblocksP2PState::default();

        let steady_peer = PeerId::random();
        let rotating_peer = PeerId::random();
        let candidate_peer = PeerId::random();
        let replacement_peer = PeerId::random();

        let steady_state = test_peer_state(false, true);
        let rotating_state = test_peer_state(false, true);
        let candidate_state = test_peer_state(true, true);
        let replacement_state = test_peer_state(true, true);

        steady_state.lock().receive_enabled = Some(Score::new(latency_window));
        rotating_state.lock().receive_enabled = Some(Score::new(latency_window));
        steady_state
            .lock()
            .receive_enabled
            .as_mut()
            .expect("steady peer score")
            .record(10);
        rotating_state
            .lock()
            .receive_enabled
            .as_mut()
            .expect("rotating peer score")
            .record(100);

        fanout.connections.insert(steady_peer, steady_state.clone());
        fanout
            .connections
            .insert(rotating_peer, rotating_state.clone());
        fanout
            .connections
            .insert(candidate_peer, candidate_state.clone());

        fanout.maybe_start_rotation(&ctx);

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
            fanout.note_peer_received_flashblock(&authorization, &flashblock, steady_peer);
        }

        assert_eq!(
            fanout.worst_receive_peer().map(|(peer_id, _)| peer_id),
            Some(candidate_peer)
        );

        fanout
            .connections
            .insert(replacement_peer, replacement_state.clone());
        fanout.maybe_start_rotation(&ctx);

        assert!(candidate_state.lock().receive_enabled.is_none());
        assert!(replacement_state.lock().request_in_flight);
        assert_eq!(fanout.awaiting_flashblocks_req, Some(replacement_peer));
    }
}
