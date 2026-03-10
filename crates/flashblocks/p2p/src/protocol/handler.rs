use crate::protocol::{
    connection::{FlashblocksConnection, FlashblocksConnectionState},
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
use rand::seq::SliceRandom;
use reth::payload::PayloadId;
use reth_eth_wire::Capability;
use reth_ethereum::network::{api::PeerId, protocol::ProtocolHandler};
use reth_network::{Peers, cache::LruCache};
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{broadcast, watch},
    time::{self, Instant},
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

/// Number of recently forwarded `StartPublish`/`StopPublish` messages we remember to avoid loops.
const CONTROL_MESSAGE_CACHE_LEN: u32 = 2048;

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

/// Runtime configuration for bounded flashblocks fanout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FanoutConfig {
    /// Maximum number of non-trusted peers to send flashblocks to.
    pub max_send_peers: usize,
    /// Maximum number of peers to receive flashblocks from.
    pub max_receive_peers: usize,
    /// How often to evaluate latency-based peer rotation.
    pub rotation_interval: Duration,
    /// How long to wait for a rotation request to be answered.
    pub rotation_timeout: Duration,
    /// Number of latency measurements to retain per receive peer.
    pub latency_window: i64,
}

impl Default for FanoutConfig {
    fn default() -> Self {
        Self {
            max_send_peers: 6,
            max_receive_peers: 6,
            rotation_interval: Duration::from_secs(30),
            rotation_timeout: Duration::from_secs(10),
            latency_window: 50,
        }
    }
}

#[derive(Clone, Debug)]
enum RotationState {
    WaitingForResponse {
        candidate: PeerId,
        evict: PeerId,
        requested_at: Instant,
    },
    WaitingForCancelAck {
        candidate: PeerId,
        evict: PeerId,
    },
}

#[derive(Debug, Default)]
struct FanoutState {
    send_set: HashSet<PeerId>,
    receive_set: HashSet<PeerId>,
    connections: HashMap<PeerId, Weak<FlashblocksConnectionState>>,
    rotation: Option<RotationState>,
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
    /// Fanout and peer-selection state for flashblock forwarding.
    fanout: FanoutState,
    /// Recently processed control messages to prevent rebroadcast loops.
    seen_control_messages: LruCache<[u8; 32]>,
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
            fanout: FanoutState::default(),
            seen_control_messages: LruCache::new(CONTROL_MESSAGE_CACHE_LEN),
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
}

impl FanoutState {
    fn connection_state(&self, peer_id: &PeerId) -> Option<Arc<FlashblocksConnectionState>> {
        self.connections.get(peer_id).and_then(Weak::upgrade)
    }

    fn is_trusted(&self, peer_id: &PeerId) -> bool {
        self.connection_state(peer_id)
            .is_some_and(|peer| peer.flags().trusted)
    }

    fn non_trusted_send_count(&self) -> usize {
        self.send_set
            .iter()
            .filter(|peer_id| !self.is_trusted(peer_id))
            .count()
    }

    fn request_in_flight_count(&self) -> usize {
        self.connections
            .values()
            .filter_map(Weak::upgrade)
            .filter(|peer| peer.flags().request_in_flight)
            .count()
    }

    fn available_receive_candidates(&self) -> Vec<PeerId> {
        let mut trusted = Vec::new();
        let mut unknown = Vec::new();
        let mut untrusted = Vec::new();

        for (peer_id, peer_state) in &self.connections {
            let Some(peer_state) = peer_state.upgrade() else {
                continue;
            };

            if self.receive_set.contains(peer_id) {
                continue;
            }

            let peer_state = peer_state.flags();
            if peer_state.request_in_flight || peer_state.cancel_in_flight {
                continue;
            }

            match (peer_state.trusted_known, peer_state.trusted) {
                (true, true) => trusted.push(*peer_id),
                (false, _) => unknown.push(*peer_id),
                (true, false) => untrusted.push(*peer_id),
            }
        }

        let mut rng = rand::rng();
        trusted.shuffle(&mut rng);
        unknown.shuffle(&mut rng);
        untrusted.shuffle(&mut rng);

        trusted.extend(unknown);
        trusted.extend(untrusted);
        trusted
    }

    fn maybe_request_receive_peers(&mut self, ctx: &FlashblocksP2PCtx) {
        if self.rotation.is_some() {
            return;
        }

        let target = ctx
            .fanout_config
            .max_receive_peers
            .saturating_sub(self.receive_set.len() + self.request_in_flight_count());
        if target == 0 {
            return;
        }

        for peer_id in self.available_receive_candidates().into_iter().take(target) {
            let Some(peer_state) = self.connection_state(&peer_id) else {
                continue;
            };

            let flags = peer_state.flags();
            if flags.request_in_flight || flags.receive_enabled || flags.cancel_in_flight {
                continue;
            }
            peer_state.update_flags(|flags| flags.request_in_flight = true);

            ctx.send_direct(peer_id, FlashblocksP2PMsg::RequestFlashblocks);
        }
    }

    fn worst_receive_peer(&self) -> Option<PeerId> {
        self.receive_set
            .iter()
            .filter_map(|peer_id| {
                self.connection_state(peer_id).and_then(|peer_state| {
                    peer_state
                        .average_latency()
                        .map(|average| (*peer_id, average))
                })
            })
            .max_by(|(_, lhs), (_, rhs)| lhs.cmp(rhs))
            .map(|(peer_id, _)| peer_id)
    }

    fn maybe_start_rotation(&mut self, ctx: &FlashblocksP2PCtx) {
        if self.rotation.is_some() || self.receive_set.len() < ctx.fanout_config.max_receive_peers {
            return;
        }

        let Some(evict) = self.worst_receive_peer() else {
            return;
        };

        let mut candidates = self.available_receive_candidates();
        if candidates.is_empty() {
            return;
        }

        let mut rng = rand::rng();
        candidates.shuffle(&mut rng);
        let candidate = candidates[0];

        let Some(candidate_state) = self.connection_state(&candidate) else {
            return;
        };

        let flags = candidate_state.flags();
        if flags.request_in_flight || flags.receive_enabled || flags.cancel_in_flight {
            return;
        }
        candidate_state.update_flags(|flags| flags.request_in_flight = true);

        self.rotation = Some(RotationState::WaitingForResponse {
            candidate,
            evict,
            requested_at: Instant::now(),
        });
        ctx.send_direct(candidate, FlashblocksP2PMsg::RequestFlashblocks);
    }

    fn check_rotation_timeout(&mut self, ctx: &FlashblocksP2PCtx) {
        let Some(RotationState::WaitingForResponse {
            candidate,
            requested_at,
            ..
        }) = self.rotation.as_ref()
        else {
            return;
        };

        if requested_at.elapsed() < ctx.fanout_config.rotation_timeout {
            return;
        }

        let candidate = *candidate;
        if let Some(candidate_state) = self.connection_state(&candidate) {
            candidate_state.update_flags(|flags| flags.request_in_flight = false);
        }
        self.rotation = None;
        self.maybe_request_receive_peers(ctx);
    }

    fn handle_disconnect(&mut self, ctx: &FlashblocksP2PCtx, peer_id: PeerId) {
        self.connections.remove(&peer_id);
        self.send_set.remove(&peer_id);
        self.receive_set.remove(&peer_id);

        if self
            .rotation
            .as_ref()
            .is_some_and(|rotation| match rotation {
                RotationState::WaitingForResponse {
                    candidate, evict, ..
                } => *candidate == peer_id || *evict == peer_id,
                RotationState::WaitingForCancelAck { candidate, evict } => {
                    *candidate == peer_id || *evict == peer_id
                }
            })
        {
            self.rotation = None;
        }

        self.maybe_request_receive_peers(ctx);
    }

    fn handle_request(&mut self, ctx: &FlashblocksP2PCtx, peer_id: PeerId) {
        let Some(peer_state) = self.connection_state(&peer_id) else {
            return;
        };

        let flags = peer_state.flags();
        let peer_is_trusted = flags.trusted;
        let send_enabled = flags.send_enabled;
        let cancel_in_flight = flags.cancel_in_flight;

        if send_enabled {
            ctx.send_direct(peer_id, FlashblocksP2PMsg::AcceptFlashblocks);
            return;
        }

        if cancel_in_flight {
            ctx.send_direct(peer_id, FlashblocksP2PMsg::RejectFlashblocks);
            return;
        }

        if peer_is_trusted {
            if self.non_trusted_send_count() >= ctx.fanout_config.max_send_peers {
                if let Some(evicted_peer) = self.send_set.iter().copied().find(|candidate| {
                    !self.is_trusted(candidate)
                        && self
                            .connection_state(candidate)
                            .is_some_and(|state| !state.flags().cancel_in_flight)
                }) {
                    if let Some(evicted_state) = self.connection_state(&evicted_peer) {
                        evicted_state.update_flags(|flags| {
                            flags.send_enabled = false;
                            flags.cancel_in_flight = true;
                        });
                    }
                    self.send_set.remove(&evicted_peer);
                    ctx.send_direct(evicted_peer, FlashblocksP2PMsg::CancelFlashblocks);
                }
            }

            peer_state.update_flags(|flags| {
                flags.send_enabled = true;
                flags.cancel_in_flight = false;
            });
            self.send_set.insert(peer_id);
            ctx.send_direct(peer_id, FlashblocksP2PMsg::AcceptFlashblocks);
            return;
        }

        if self.non_trusted_send_count() < ctx.fanout_config.max_send_peers {
            peer_state.update_flags(|flags| {
                flags.send_enabled = true;
                flags.cancel_in_flight = false;
            });
            self.send_set.insert(peer_id);
            ctx.send_direct(peer_id, FlashblocksP2PMsg::AcceptFlashblocks);
        } else {
            ctx.send_direct(peer_id, FlashblocksP2PMsg::RejectFlashblocks);
        }
    }

    fn handle_accept(&mut self, ctx: &FlashblocksP2PCtx, peer_id: PeerId) {
        let Some(peer_state) = self.connection_state(&peer_id) else {
            return;
        };

        if !peer_state.flags().request_in_flight {
            return;
        }
        peer_state.update_flags(|flags| {
            flags.request_in_flight = false;
            flags.receive_enabled = true;
        });

        self.receive_set.insert(peer_id);

        if let Some(RotationState::WaitingForResponse {
            candidate, evict, ..
        }) = self.rotation.as_ref()
        {
            if *candidate == peer_id {
                let evict = *evict;
                if let Some(evict_state) = self.connection_state(&evict) {
                    evict_state.update_flags(|flags| flags.cancel_in_flight = true);
                } else {
                    self.rotation = None;
                    self.maybe_request_receive_peers(ctx);
                    return;
                }
                self.rotation = Some(RotationState::WaitingForCancelAck {
                    candidate: peer_id,
                    evict,
                });
                ctx.send_direct(evict, FlashblocksP2PMsg::CancelFlashblocks);
                return;
            }
        }

        self.maybe_request_receive_peers(ctx);
    }

    fn handle_reject(&mut self, ctx: &FlashblocksP2PCtx, peer_id: PeerId) {
        let Some(peer_state) = self.connection_state(&peer_id) else {
            return;
        };

        if !peer_state.flags().request_in_flight {
            return;
        }
        peer_state.update_flags(|flags| flags.request_in_flight = false);

        if self.rotation.as_ref().is_some_and(|rotation| {
            matches!(
                rotation,
                RotationState::WaitingForResponse { candidate, .. } if *candidate == peer_id
            )
        }) {
            self.rotation = None;
        }

        self.maybe_request_receive_peers(ctx);
    }

    fn handle_cancel(&mut self, ctx: &FlashblocksP2PCtx, peer_id: PeerId) {
        self.send_set.remove(&peer_id);
        self.receive_set.remove(&peer_id);

        if let Some(peer_state) = self.connection_state(&peer_id) {
            peer_state.update_flags(|flags| {
                flags.send_enabled = false;
                flags.receive_enabled = false;
                flags.request_in_flight = false;
                flags.cancel_in_flight = false;
            });
            peer_state.reset_latency();
        }

        if self
            .rotation
            .as_ref()
            .is_some_and(|rotation| match rotation {
                RotationState::WaitingForResponse {
                    candidate, evict, ..
                } => *candidate == peer_id || *evict == peer_id,
                RotationState::WaitingForCancelAck { candidate, evict } => {
                    *candidate == peer_id || *evict == peer_id
                }
            })
        {
            self.rotation = None;
        }

        ctx.send_direct(peer_id, FlashblocksP2PMsg::CancelFlashblocksAck);
        self.maybe_request_receive_peers(ctx);
    }

    fn handle_cancel_ack(&mut self, ctx: &FlashblocksP2PCtx, peer_id: PeerId) {
        let Some(peer_state) = self.connection_state(&peer_id) else {
            return;
        };

        let clear_rotation = self.rotation.as_ref().is_some_and(|rotation| {
            matches!(
                rotation,
                RotationState::WaitingForCancelAck { evict, .. } if *evict == peer_id
            )
        });

        if !peer_state.flags().cancel_in_flight {
            return;
        }
        peer_state.update_flags(|flags| {
            flags.cancel_in_flight = false;
            flags.request_in_flight = false;

            if clear_rotation {
                flags.receive_enabled = false;
            } else {
                flags.send_enabled = false;
            }
        });

        if clear_rotation {
            peer_state.reset_latency();
            self.rotation = None;
            self.receive_set.remove(&peer_id);
        } else {
            self.send_set.remove(&peer_id);
        }

        self.maybe_request_receive_peers(ctx);
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
    /// Ensures rotation/background tasks are only started once per handle.
    background_tasks_started: Arc<AtomicBool>,
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
            background_tasks_started: Arc::new(AtomicBool::new(false)),
        };

        Self { ctx, state }
    }

    pub fn flashblocks_tx(&self) -> broadcast::Sender<FlashblocksPayloadV1> {
        self.ctx.flashblock_tx.clone()
    }

    pub fn builder_sk(&self) -> Result<&SigningKey, FlashblocksP2PError> {
        self.ctx
            .builder_sk
            .as_ref()
            .ok_or(FlashblocksP2PError::MissingBuilderSk)
    }

    pub(crate) fn ensure_background_tasks(&self) {
        if self
            .ctx
            .background_tasks_started
            .swap(true, Ordering::AcqRel)
        {
            return;
        }

        let handle = self.clone();
        tokio::spawn(async move {
            let mut rotation_interval = time::interval(handle.ctx.fanout_config.rotation_interval);
            let mut timeout_interval = time::interval(Duration::from_secs(1));

            loop {
                tokio::select! {
                    _ = rotation_interval.tick() => {
                        let mut state = handle.state.lock();
                        state.fanout.maybe_start_rotation(&handle.ctx);
                    }
                    _ = timeout_interval.tick() => {
                        let mut state = handle.state.lock();
                        state.fanout.check_rotation_timeout(&handle.ctx);
                    }
                }
            }
        });
    }

    pub(crate) fn on_peer_connected<N: FlashblocksP2PNetworkHandle>(
        &self,
        network: N,
        peer_id: PeerId,
        fanout_state: Arc<FlashblocksConnectionState>,
    ) {
        {
            let mut state = self.state.lock();
            state
                .fanout
                .connections
                .insert(peer_id, Arc::downgrade(&fanout_state));
            state.fanout.maybe_request_receive_peers(&self.ctx);
        }

        let handle = self.clone();
        tokio::spawn(async move {
            match network.get_peer_by_id(peer_id).await {
                Ok(Some(peer_info)) => {
                    fanout_state.update_flags(|flags| {
                        flags.trusted = peer_info.kind.is_trusted();
                        flags.trusted_known = true;
                    });

                    let mut state = handle.state.lock();
                    if state
                        .fanout
                        .connections
                        .get(&peer_id)
                        .and_then(Weak::upgrade)
                        .is_some_and(|current| Arc::ptr_eq(&current, &fanout_state))
                    {
                        state.fanout.maybe_request_receive_peers(&handle.ctx);
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
        state.fanout.handle_disconnect(&self.ctx, peer_id);
    }

    pub(crate) fn remember_control_message(&self, authorized: &Authorized) -> bool {
        let encoded = FlashblocksP2PMsg::Authorized(authorized.clone()).encode();
        let hash = blake3::hash(&encoded);
        self.state
            .lock()
            .seen_control_messages
            .insert(*hash.as_bytes())
    }

    pub(crate) fn handle_request_message(&self, peer_id: PeerId) {
        let mut state = self.state.lock();
        state.fanout.handle_request(&self.ctx, peer_id);
    }

    pub(crate) fn handle_accept_message(&self, peer_id: PeerId) {
        let mut state = self.state.lock();
        state.fanout.handle_accept(&self.ctx, peer_id);
    }

    pub(crate) fn handle_reject_message(&self, peer_id: PeerId) {
        let mut state = self.state.lock();
        state.fanout.handle_reject(&self.ctx, peer_id);
    }

    pub(crate) fn handle_cancel_message(&self, peer_id: PeerId) {
        let mut state = self.state.lock();
        state.fanout.handle_cancel(&self.ctx, peer_id);
    }

    pub(crate) fn handle_cancel_ack_message(&self, peer_id: PeerId) {
        let mut state = self.state.lock();
        state.fanout.handle_cancel_ack(&self.ctx, peer_id);
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
        self.ctx.publish(&mut state, authorized_payload);
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
    /// - Broadcasts to peers and publishes ordered flashblocks to the stream
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
        let fanout_state = Arc::new(FlashblocksConnectionState::new(
            self.handle.ctx.fanout_config.latency_window,
        ));

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
            background_tasks_started: Arc::new(AtomicBool::new(false)),
        }
    }

    fn test_peer_state(
        latency_window: i64,
        trusted: bool,
        trusted_known: bool,
    ) -> Arc<FlashblocksConnectionState> {
        let state = Arc::new(FlashblocksConnectionState::new(latency_window));
        state.update_flags(|flags| {
            flags.trusted = trusted;
            flags.trusted_known = trusted_known;
        });
        state
    }

    #[test]
    fn trusted_peers_are_requested_first() {
        let config = FanoutConfig {
            max_receive_peers: 1,
            ..Default::default()
        };
        let latency_window = config.latency_window;
        let ctx = test_ctx(config);
        let mut fanout = FanoutState::default();
        let mut rx = ctx.peer_tx.subscribe();

        let trusted_peer = PeerId::random();
        let untrusted_peer = PeerId::random();
        let trusted_state = test_peer_state(latency_window, true, true);
        let untrusted_state = test_peer_state(latency_window, false, true);
        fanout
            .connections
            .insert(trusted_peer, Arc::downgrade(&trusted_state));
        fanout
            .connections
            .insert(untrusted_peer, Arc::downgrade(&untrusted_state));

        fanout.maybe_request_receive_peers(&ctx);

        assert!(trusted_state.flags().request_in_flight);
        assert!(!untrusted_state.flags().request_in_flight);
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
    fn trusted_request_evicts_non_trusted_sender() {
        let config = FanoutConfig {
            max_send_peers: 1,
            ..Default::default()
        };
        let latency_window = config.latency_window;
        let ctx = test_ctx(config);
        let mut fanout = FanoutState::default();
        let mut rx = ctx.peer_tx.subscribe();

        let victim = PeerId::random();
        let trusted_requester = PeerId::random();
        let victim_state = test_peer_state(latency_window, false, true);
        let requester_state = test_peer_state(latency_window, true, true);
        victim_state.update_flags(|flags| flags.send_enabled = true);
        fanout
            .connections
            .insert(victim, Arc::downgrade(&victim_state));
        fanout
            .connections
            .insert(trusted_requester, Arc::downgrade(&requester_state));
        fanout.send_set.insert(victim);

        fanout.handle_request(&ctx, trusted_requester);

        assert!(!fanout.send_set.contains(&victim));
        assert!(fanout.send_set.contains(&trusted_requester));
        assert!(victim_state.flags().cancel_in_flight);
        assert!(requester_state.flags().send_enabled);

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
    fn rotation_accepts_candidate_then_waits_for_cancel_ack() {
        let config = FanoutConfig {
            max_receive_peers: 1,
            latency_window: 4,
            ..Default::default()
        };
        let latency_window = config.latency_window;
        let ctx = test_ctx(config);
        let mut fanout = FanoutState::default();
        let mut rx = ctx.peer_tx.subscribe();

        let current_peer = PeerId::random();
        let candidate_peer = PeerId::random();
        let current_state = test_peer_state(latency_window, false, true);
        let candidate_state = test_peer_state(latency_window, false, true);
        current_state.update_flags(|flags| flags.receive_enabled = true);
        current_state.record_latency(42);
        fanout
            .connections
            .insert(current_peer, Arc::downgrade(&current_state));
        fanout
            .connections
            .insert(candidate_peer, Arc::downgrade(&candidate_state));
        fanout.receive_set.insert(current_peer);

        fanout.maybe_start_rotation(&ctx);

        assert!(candidate_state.flags().request_in_flight);
        assert!(matches!(
            fanout.rotation,
            Some(RotationState::WaitingForResponse { candidate, evict, .. })
                if candidate == candidate_peer && evict == current_peer
        ));

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

        assert!(fanout.receive_set.contains(&current_peer));
        assert!(fanout.receive_set.contains(&candidate_peer));
        assert!(candidate_state.flags().receive_enabled);
        assert!(current_state.flags().cancel_in_flight);
        assert!(matches!(
            fanout.rotation,
            Some(RotationState::WaitingForCancelAck { candidate, evict })
                if candidate == candidate_peer && evict == current_peer
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

        fanout.handle_cancel_ack(&ctx, current_peer);

        assert!(!fanout.receive_set.contains(&current_peer));
        assert!(fanout.receive_set.contains(&candidate_peer));
        assert!(!current_state.flags().receive_enabled);
        assert!(fanout.rotation.is_none());
    }
}
