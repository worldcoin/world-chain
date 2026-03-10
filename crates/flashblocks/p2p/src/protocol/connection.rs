use crate::protocol::handler::{
    FlashblocksP2PNetworkHandle, FlashblocksP2PProtocol, MAX_FLASHBLOCK_INDEX, PeerMsg,
    PublishingStatus,
};
use alloy_primitives::bytes::BytesMut;
use chrono::Utc;
use flashblocks_primitives::{
    p2p::{
        Authorized, AuthorizedMsg, AuthorizedPayload, FlashblocksP2PMsg, StartPublish, StopPublish,
    },
    primitives::FlashblocksPayloadV1,
};
use futures::{Stream, StreamExt};
use metrics::gauge;
use parking_lot::Mutex;
use reth::payload::PayloadId;
use reth_ethereum::network::{api::PeerId, eth_wire::multiplex::ProtocolConnection};
use reth_network::{cache::LruMap, types::ReputationChangeKind};
use std::{
    fmt,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, ready},
};
use tokio_stream::wrappers::BroadcastStream;
use tracing::{info, trace};

/// Grace period for authorization timestamp checks to reduce false positives from
/// minor skew/races between peers.
const AUTHORIZATION_TIMESTAMP_GRACE_SEC: u64 = 10;

/// Number of payload receive-sets cached per peer.
///
/// This should be large enough to retain entries across the grace window.
const RECEIVED_CACHE_LEN: u32 = AUTHORIZATION_TIMESTAMP_GRACE_SEC as u32 * 20;

/// A lightweight moving average with a configurable smoothing window.
#[derive(Clone, Debug)]
pub struct MovingAverage {
    value: Option<i64>,
    window: i64,
}

impl MovingAverage {
    pub(crate) fn new(window: i64) -> Self {
        Self {
            value: None,
            window: window.max(1),
        }
    }

    pub(crate) fn record(&mut self, sample: i64) {
        self.value = Some(match self.value {
            Some(current) => (current * (self.window - 1) + sample) / self.window,
            None => sample,
        });
    }

    pub(crate) fn value(&self) -> Option<i64> {
        self.value
    }

    pub(crate) fn reset(&mut self) {
        self.value = None;
    }
}

/// Shared connection metadata for a single peer connection.
pub struct FlashblocksConnectionState {
    pub trusted: bool,
    pub trusted_known: bool,
    pub send_enabled: bool,
    pub receive_enabled: bool,
    pub request_in_flight: bool,
    score_average: MovingAverage,
    received_cache: LruMap<(PayloadId, usize), ()>,
}

impl fmt::Debug for FlashblocksConnectionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FlashblocksConnectionState")
            .field("trusted", &self.trusted)
            .field("trusted_known", &self.trusted_known)
            .field("send_enabled", &self.send_enabled)
            .field("receive_enabled", &self.receive_enabled)
            .field("request_in_flight", &self.request_in_flight)
            .field("score_average", &self.score_average)
            .finish()
    }
}

impl FlashblocksConnectionState {
    pub(crate) fn new(latency_window: i64) -> Self {
        Self {
            trusted: false,
            trusted_known: false,
            send_enabled: false,
            receive_enabled: false,
            request_in_flight: false,
            score_average: MovingAverage::new(latency_window),
            received_cache: LruMap::new(RECEIVED_CACHE_LEN),
        }
    }

    pub(crate) fn record_latency(&mut self, sample: i64) {
        self.score_average.record(sample);
    }

    pub(crate) fn record_missed_flashblock(&mut self, penalty: i64) {
        self.score_average.record(penalty);
    }

    pub(crate) fn score(&self) -> Option<i64> {
        self.score_average.value()
    }

    pub(crate) fn note_received_flashblock(&mut self, key: (PayloadId, usize)) -> bool {
        if self.received_cache.peek(&key).is_some() {
            return false;
        }
        self.received_cache.insert(key, ())
    }

    pub(crate) fn has_received_flashblock(&self, key: &(PayloadId, usize)) -> bool {
        self.received_cache.peek(key).is_some()
    }

    pub(crate) fn reset_receive_tracking(&mut self) {
        self.score_average.reset();
        self.received_cache = LruMap::new(RECEIVED_CACHE_LEN);
    }
}

/// Represents a single P2P connection for the flashblocks protocol.
///
/// This struct manages the bidirectional communication with a single peer in the flashblocks
/// P2P network. It handles incoming messages from the peer, validates and processes them,
/// and also streams outgoing messages that need to be broadcast.
///
/// The connection implements the `Stream` trait to provide outgoing message bytes that
/// should be sent to the connected peer over the underlying protocol connection.
pub struct FlashblocksConnection<N> {
    /// The flashblocks protocol handler that manages the overall protocol state.
    protocol: FlashblocksP2PProtocol<N>,
    /// The underlying protocol connection for sending and receiving raw bytes.
    conn: ProtocolConnection,
    /// The unique identifier of the connected peer.
    peer_id: PeerId,
    /// Receiver for peer messages to be sent to all peers.
    /// We send bytes over this stream to avoid repeatedly having to serialize the payloads.
    peer_rx: BroadcastStream<PeerMsg>,
    /// Shared connection state for this peer, also visible to the protocol handler.
    state: Arc<Mutex<FlashblocksConnectionState>>,
}

impl<N: FlashblocksP2PNetworkHandle> FlashblocksConnection<N> {
    /// Creates a new `FlashblocksConnection` instance.
    ///
    /// # Arguments
    /// * `protocol` - The flashblocks protocol handler managing the connection.
    /// * `conn` - The underlying protocol connection for sending and receiving messages.
    /// * `peer_id` - The unique identifier of the connected peer.
    /// * `peer_rx` - Receiver for peer messages to be sent to all peers.
    pub(crate) fn new(
        protocol: FlashblocksP2PProtocol<N>,
        conn: ProtocolConnection,
        peer_id: PeerId,
        peer_rx: BroadcastStream<PeerMsg>,
        state: Arc<Mutex<FlashblocksConnectionState>>,
    ) -> Self {
        protocol.handle.ensure_background_tasks();
        protocol
            .handle
            .on_peer_connected(protocol.network.clone(), peer_id, state.clone());

        gauge!("flashblocks.peers", "capability" => FlashblocksP2PProtocol::<N>::capability().to_string()).increment(1);

        Self {
            protocol,
            conn,
            peer_id,
            peer_rx,
            state,
        }
    }
}

impl<N> Drop for FlashblocksConnection<N> {
    fn drop(&mut self) {
        info!(
            target: "flashblocks::p2p",
            peer_id = %self.peer_id,
            "dropping flashblocks connection"
        );

        self.protocol.handle.on_peer_disconnected(self.peer_id);
        gauge!("flashblocks.peers", "capability" => FlashblocksP2PProtocol::<N>::capability().to_string()).decrement(1);
    }
}

impl<N: FlashblocksP2PNetworkHandle> Stream for FlashblocksConnection<N> {
    type Item = BytesMut;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            // Check if there are any flashblocks ready to broadcast to our peers.
            if let Poll::Ready(Some(res)) = this.peer_rx.poll_next_unpin(cx) {
                match res {
                    Ok(peer_msg) => {
                        match peer_msg {
                            PeerMsg::FlashblocksPayloadV1((
                                payload_id,
                                flashblock_index,
                                bytes,
                            )) => {
                                // Check if this flashblock actually originated from this peer.
                                let should_send = {
                                    let state = this.state.lock();
                                    state.send_enabled
                                        && !state.has_received_flashblock(&(
                                            payload_id,
                                            flashblock_index,
                                        ))
                                };
                                if should_send {
                                    trace!(
                                        target: "flashblocks::p2p",
                                        peer_id = %this.peer_id,
                                        %payload_id,
                                        %flashblock_index,
                                        "Broadcasting `FlashblocksPayloadV1` message to peer"
                                    );
                                    metrics::counter!("flashblocks.bandwidth_outbound")
                                        .increment(bytes.len() as u64);

                                    return Poll::Ready(Some(bytes));
                                }
                            }
                            PeerMsg::StartPublishing(bytes_mut) => {
                                trace!(
                                    target: "flashblocks::p2p",
                                    peer_id = %this.peer_id,
                                    "Broadcasting `StartPublishing` to peer"
                                );
                                return Poll::Ready(Some(bytes_mut));
                            }
                            PeerMsg::StopPublishing(bytes_mut) => {
                                trace!(
                                    target: "flashblocks::p2p",
                                    peer_id = %this.peer_id,
                                    "Broadcasting `StopPublishing` to peer"
                                );
                                return Poll::Ready(Some(bytes_mut));
                            }
                            PeerMsg::Direct { peer_id, bytes } => {
                                if peer_id == this.peer_id {
                                    trace!(
                                        target: "flashblocks::p2p",
                                        peer_id = %this.peer_id,
                                        "Sending direct flashblocks control message to peer"
                                    );
                                    return Poll::Ready(Some(bytes));
                                }
                            }
                        }
                    }
                    Err(error) => {
                        tracing::error!(
                            target: "flashblocks::p2p",
                            %error,
                            "failed to receive flashblocks message from peer_rx"
                        );
                    }
                }
            }

            // Check if there are any messages from the peer.
            let Some(buf) = ready!(this.conn.poll_next_unpin(cx)) else {
                return Poll::Ready(None);
            };

            let msg = match FlashblocksP2PMsg::decode(&mut &buf[..]) {
                Ok(msg) => msg,
                Err(error) => {
                    tracing::warn!(
                        target: "flashblocks::p2p",
                        peer_id = %this.peer_id,
                        %error,
                        "failed to decode flashblocks message from peer",
                    );
                    this.protocol
                        .network
                        .reputation_change(this.peer_id, ReputationChangeKind::BadMessage);
                    return Poll::Ready(None);
                }
            };

            match msg {
                FlashblocksP2PMsg::Authorized(authorized) => {
                    if Ok(authorized.authorization.builder_vk)
                        == this.protocol.handle.builder_sk().map(|s| s.verifying_key())
                    {
                        tracing::trace!(
                            target: "flashblocks::p2p",
                            peer_id = %this.peer_id,
                            "received our own message from peer",
                        );
                        continue;
                    }

                    if let Err(error) = authorized.verify(this.protocol.handle.ctx.authorizer_vk) {
                        tracing::warn!(
                            target: "flashblocks::p2p",
                            peer_id = %this.peer_id,
                            %error,
                            "failed to verify flashblock",
                        );
                        this.protocol
                            .network
                            .reputation_change(this.peer_id, ReputationChangeKind::BadMessage);
                        continue;
                    }

                    match &authorized.msg {
                        AuthorizedMsg::FlashblocksPayloadV1(_) => {
                            metrics::counter!("flashblocks.bandwidth_inbound")
                                .increment(buf.len() as u64);
                            this.handle_flashblocks_payload_v1(authorized.into_unchecked());
                        }
                        AuthorizedMsg::StartPublish(_) => {
                            this.handle_start_publish(authorized.into_unchecked());
                        }
                        AuthorizedMsg::StopPublish(_) => {
                            this.handle_stop_publish(authorized.into_unchecked());
                        }
                    }
                }
                FlashblocksP2PMsg::RequestFlashblocks => {
                    this.protocol.handle.handle_request_message(this.peer_id);
                }
                FlashblocksP2PMsg::AcceptFlashblocks => {
                    this.protocol.handle.handle_accept_message(this.peer_id);
                }
                FlashblocksP2PMsg::RejectFlashblocks => {
                    this.protocol.handle.handle_reject_message(this.peer_id);
                }
                FlashblocksP2PMsg::CancelFlashblocks => {
                    this.protocol.handle.handle_cancel_message(this.peer_id);
                }
            }
        }
    }
}

impl<N: FlashblocksP2PNetworkHandle> FlashblocksConnection<N> {
    /// Handles incoming flashblock payload messages from a peer.
    ///
    /// This method validates the flashblock payload, checks for duplicates and ordering,
    /// updates the active publisher tracking, and forwards valid payloads for processing.
    /// It also manages peer reputation based on message validity and prevents spam attacks.
    ///
    /// # Arguments
    /// * `authorized_payload` - The authorized flashblock payload received from the peer
    ///
    /// # Behavior
    /// - Validates timestamp to prevent replay attacks
    /// - Tracks duplicate detection across recently seen payloads
    /// - Prevents duplicate flashblock spam from the same peer
    /// - Updates active publisher information from base payload data
    /// - Forwards valid payloads to the protocol handler for processing
    fn handle_flashblocks_payload_v1(
        &mut self,
        authorized_payload: AuthorizedPayload<FlashblocksPayloadV1>,
    ) {
        let state_handle = self.protocol.handle.state.clone();
        let mut state = state_handle.lock();
        let authorization = &authorized_payload.authorized.authorization;
        let msg = authorized_payload.msg();

        // Check if this payload is older than our current view by more than the allowed
        // grace window.
        if authorization.timestamp
            < state
                .payload_timestamp
                .saturating_sub(AUTHORIZATION_TIMESTAMP_GRACE_SEC)
        {
            tracing::warn!(
                target: "flashblocks::p2p",
                peer_id = %self.peer_id,
                current_timestamp = state.payload_timestamp,
                timestamp = authorization.timestamp,
                grace_sec = AUTHORIZATION_TIMESTAMP_GRACE_SEC,
                "received flashblock with outdated timestamp",
            );
            self.protocol
                .network
                .reputation_change(self.peer_id, ReputationChangeKind::BadMessage);
            return;
        }

        // Check if the payload index is within the allowed range
        if msg.index as usize > MAX_FLASHBLOCK_INDEX {
            tracing::error!(
                target: "flashblocks::p2p",
                peer_id = %self.peer_id,
                index = msg.index,
                payload_id = %msg.payload_id,
                max_index = MAX_FLASHBLOCK_INDEX,
                "Received flashblocks payload with index exceeding maximum"
            );
            return;
        }

        if !self.state.lock().receive_enabled {
            trace!(
                target: "flashblocks::p2p",
                peer_id = %self.peer_id,
                payload_id = %msg.payload_id,
                index = msg.index,
                "ignoring flashblock from peer outside receive set",
            );
            return;
        }

        // Check if this peer is spamming us with the same payload index
        if !self
            .state
            .lock()
            .note_received_flashblock((msg.payload_id, msg.index as usize))
        {
            // We've already seen this index from this peer.
            // They could be trying to DOS us.
            tracing::warn!(
                target: "flashblocks::p2p",
                peer_id = %self.peer_id,
                payload_id = %msg.payload_id,
                index = msg.index,
                "received duplicate flashblock from peer",
            );
            self.protocol
                .network
                .reputation_change(self.peer_id, ReputationChangeKind::AlreadySeenTransaction);
            return;
        }

        state.publishing_status.send_modify(|status| {
            let active_publishers = match status {
                PublishingStatus::Publishing { .. } => {
                    // We are currently building, so we should not be seeing any new flashblocks
                    // over the p2p network.
                    tracing::error!(
                        target: "flashblocks::p2p",
                        peer_id = %self.peer_id,
                        "received flashblock while already building",
                    );
                    return;
                }
                PublishingStatus::WaitingToPublish {
                    active_publishers, ..
                } => active_publishers,
                PublishingStatus::NotPublishing { active_publishers } => active_publishers,
            };

            // Update the list of active publishers
            if let Some((_, timestamp)) = active_publishers
                .iter_mut()
                .find(|(publisher, _)| *publisher == authorization.builder_vk)
            {
                // This is an existing publisher, we should update their block number
                *timestamp = authorization.timestamp;
            } else {
                // This is a new publisher, we should add them to the list of active publishers
                active_publishers.push((authorization.builder_vk, authorization.timestamp));
            }
        });

        let now = Utc::now()
            .timestamp_nanos_opt()
            .expect("time went backwards");

        if let Some(flashblock_timestamp) = msg.metadata.flashblock_timestamp {
            let latency = now - flashblock_timestamp;
            metrics::histogram!("flashblocks.latency").record(latency as f64 / 1_000_000_000.0);
            self.state.lock().record_latency(latency);
        }

        self.protocol
            .handle
            .ctx
            .publish(&mut state, authorized_payload, Some(self.peer_id));
    }

    /// Handles incoming `StartPublish` messages from a peer.
    ///
    /// # Arguments
    /// * `authorized_payload` - The authorized `StartPublish` message received from the peer
    ///
    /// # Behavior
    /// - Validates the timestamp to prevent replay attacks
    /// - Updates the publishing status to reflect the new publisher
    /// - If we are currently publishing, sends a `StopPublish` message to ourselves
    /// - If we are waiting to publish, updates the list of active publishers
    /// - If we are not publishing, adds the new publisher to the list of active publishers
    fn handle_start_publish(&mut self, authorized_payload: AuthorizedPayload<StartPublish>) {
        let Ok(builder_sk) = self.protocol.handle.builder_sk() else {
            return;
        };
        let authorization = &authorized_payload.authorized.authorization;
        let payload_timestamp = self.protocol.handle.state.lock().payload_timestamp;

        // Check if the request is expired for dos protection.
        // It's important to ensure that this `StartPublish` request
        // is very recent, or it could be used in a replay attack.
        if payload_timestamp > authorization.timestamp {
            tracing::warn!(
                target: "flashblocks::p2p",
                peer_id = %self.peer_id,
                current_timestamp = payload_timestamp,
                timestamp = authorized_payload.authorized.authorization.timestamp,
                "received initiate build request with outdated timestamp",
            );
            self.protocol
                .network
                .reputation_change(self.peer_id, ReputationChangeKind::BadMessage);
            return;
        }

        let state = self.protocol.handle.state.lock();
        state.publishing_status.send_modify(|status| {
            let active_publishers = match status {
                PublishingStatus::Publishing {
                    authorization: our_authorization,
                } => {
                    tracing::info!(
                        target: "flashblocks::p2p",
                        peer_id = %self.peer_id,
                        "Received StartPublish over p2p, stopping publishing flashblocks"
                    );

                    let authorized =
                        Authorized::new(builder_sk, *our_authorization, StopPublish.into());
                    let p2p_msg = FlashblocksP2PMsg::Authorized(authorized);
                    let peer_msg = PeerMsg::StopPublishing(p2p_msg.encode());
                    self.protocol.handle.ctx.peer_tx.send(peer_msg).ok();

                    *status = PublishingStatus::NotPublishing {
                        active_publishers: vec![(
                            authorization.builder_vk,
                            authorization.timestamp,
                        )],
                    };

                    return;
                }
                PublishingStatus::WaitingToPublish {
                    active_publishers, ..
                } => {
                    // We are currently waiting to build, but someone else is requesting to build
                    // This could happen during a double failover.
                    // We have a potential race condition here so we'll just wait for the
                    // build request override to kick in next block.
                    tracing::warn!(
                        target: "flashblocks::p2p",
                        peer_id = %self.peer_id,
                        "Received StartPublish over p2p while already waiting to publish, ignoring",
                    );
                    active_publishers
                }
                PublishingStatus::NotPublishing { active_publishers } => active_publishers,
            };

            if let Some((_, timestamp)) = active_publishers
                .iter_mut()
                .find(|(publisher, _)| *publisher == authorization.builder_vk)
            {
                // This is an existing publisher, we should update their block number
                *timestamp = authorization.timestamp;
            } else {
                // This is a new publisher, we should add them to the list of active publishers
                active_publishers.push((authorization.builder_vk, authorization.timestamp));
            }
        });

        let p2p_msg = FlashblocksP2PMsg::Authorized(authorized_payload.authorized.clone());
        self.protocol
            .handle
            .ctx
            .peer_tx
            .send(PeerMsg::StartPublishing(p2p_msg.encode()))
            .ok();
    }

    /// Handles incoming `StopPublish` messages from a peer.

    /// # Arguments
    /// * `authorized_payload` - The authorized `StopPublish` message received from the peer
    ///
    /// # Behavior
    /// - Validates the timestamp to prevent replay attacks
    /// - Updates the publishing status based on the current state
    /// - If we are currently publishing, logs a warning
    /// - If we are waiting to publish, removes the publisher from the list of active publishers and checks if we can start publishing
    /// - If we are not publishing, removes the publisher from the list of active publishers
    fn handle_stop_publish(&mut self, authorized_payload: AuthorizedPayload<StopPublish>) {
        let authorization = &authorized_payload.authorized.authorization;
        let payload_timestamp = self.protocol.handle.state.lock().payload_timestamp;

        // Check if the request is expired for dos protection.
        // It's important to ensure that this `StartPublish` request
        // is very recent, or it could be used in a replay attack.
        if payload_timestamp > authorization.timestamp {
            tracing::warn!(
                target: "flashblocks::p2p",
                peer_id = %self.peer_id,
                current_timestamp = payload_timestamp,
                timestamp = authorized_payload.authorized.authorization.timestamp,
                "Received initiate build response with outdated timestamp",
            );
            self.protocol
                .network
                .reputation_change(self.peer_id, ReputationChangeKind::BadMessage);
            return;
        }

        let state = self.protocol.handle.state.lock();
        state.publishing_status.send_modify(|status| {
            match status {
                PublishingStatus::Publishing { .. } => {
                    tracing::warn!(
                        target: "flashblocks::p2p",
                        peer_id = %self.peer_id,
                        "Received StopPublish over p2p while we are the publisher"
                    );
                }
                PublishingStatus::WaitingToPublish {
                    active_publishers,
                    authorization,
                    ..
                } => {
                    // We are currently waiting to build, and someone else is requesting to stop building.
                    tracing::info!(
                        target: "flashblocks::p2p",
                        peer_id = %self.peer_id,
                        "Received StopPublish over p2p while waiting to publish",
                    );

                    // Remove the publisher from the list of active publishers
                    if let Some(index) = active_publishers.iter().position(|(publisher, _)| {
                        *publisher == authorized_payload.authorized.authorization.builder_vk
                    }) {
                        active_publishers.remove(index);
                    } else {
                        tracing::warn!(
                            target: "flashblocks::p2p",
                            peer_id = %self.peer_id,
                            "Received StopPublish for unknown publisher",
                        );
                    }

                    if active_publishers.is_empty() {
                        // If there are no active publishers left, we should stop waiting to publish
                        tracing::info!(
                            target: "flashblocks::p2p",
                            peer_id = %self.peer_id,
                            "starting to publish"
                        );
                        *status = PublishingStatus::Publishing {
                            authorization: *authorization,
                        };
                    } else {
                        tracing::info!(
                            target: "flashblocks::p2p",
                            peer_id = %self.peer_id,
                            "still waiting on active publishers",
                        );
                    }
                }
                PublishingStatus::NotPublishing { active_publishers } => {
                    // Remove the publisher from the list of active publishers
                    if let Some(index) = active_publishers.iter().position(|(publisher, _)| {
                        *publisher == authorized_payload.authorized.authorization.builder_vk
                    }) {
                        active_publishers.remove(index);
                    } else {
                        tracing::warn!(
                            target: "flashblocks::p2p",
                            peer_id = %self.peer_id,
                            "Received StopPublish for unknown publisher",
                        );
                    }
                }
            }
        });

        let p2p_msg = FlashblocksP2PMsg::Authorized(authorized_payload.authorized.clone());
        self.protocol
            .handle
            .ctx
            .peer_tx
            .send(PeerMsg::StopPublishing(p2p_msg.encode()))
            .ok();
    }
}
