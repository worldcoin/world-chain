use crate::protocol::handler::{
    FlashblocksP2PNetworkHandle, FlashblocksP2PProtocol, MAX_FLASHBLOCK_INDEX, PublishingStatus,
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
use reth_ethereum::network::{api::PeerId, eth_wire::multiplex::ProtocolConnection};
use reth_network::types::ReputationChangeKind;
use std::{
    pin::Pin,
    task::{Context, Poll, ready},
    time::Instant,
};
use tokio::sync::mpsc;
use tracing::{info, trace};

/// Grace period for authorization timestamp checks to reduce false positives from
/// minor skew/races between peers.
const AUTHORIZATION_TIMESTAMP_GRACE_SEC: u64 = 10;

/// Shared connection metadata for a single peer connection.
#[derive(Clone, Debug)]
pub struct FlashblocksConnectionState {
    /// Whether this peer is marked as trusted or not.
    pub trusted: bool,
    /// Whether we currently have an outstanding flashblocks request to this peer.
    pub request_in_flight: bool,
    /// Whether we intentionally abandoned an in-flight request and should treat a late
    /// Accept/Reject as stale instead of malicious.
    pub abandoned_request_in_flight: bool,
    /// Whether we are currently sending flashblocks to this peer.
    pub send_enabled: bool,
    /// Whether we are currently requesting flashblocks from this peer.
    ///
    /// Optional score for this peer connection, used for adaptive timeouts and peer selection.
    /// Lower is better. Corresponds the moving average of flashblock latency, with missed blocks
    /// counting as 10s. While `request_in_flight` is true, the peer is only a provisional
    /// candidate and must not deliver flashblocks yet.
    pub receive_enabled: Option<Score>,
    /// Timestamp of the last receive-side state transition for this peer.
    /// Used for late-message grace checks and receive retry cooldown.
    pub receive_enabled_timestamp: u64,
    /// Per-peer channel for sending serialized protocol messages to this peer.
    pub outbound_tx: Option<mpsc::UnboundedSender<BytesMut>>,
    /// Number of control messages received in the current rate-limit window.
    pub control_msg_count: u32,
    /// Start of the current rate-limit window.
    pub control_msg_window_start: Instant,
}

impl FlashblocksConnectionState {
    pub(crate) fn new() -> Self {
        Self {
            trusted: false,
            request_in_flight: false,
            abandoned_request_in_flight: false,
            send_enabled: false,
            receive_enabled: None,
            receive_enabled_timestamp: 0,
            outbound_tx: None,
            control_msg_count: 0,
            control_msg_window_start: Instant::now(),
        }
    }
}

/// Represents a single P2P connection for the flashblocks protocol.
///
/// This struct manages the bidirectional communication with a single peer in the flashblocks
/// P2P network. It handles incoming messages from the peer, validates and processes them,
/// and also streams serialized outgoing messages queued for this peer.
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
    /// Receiver for already serialized protocol messages targeted at this specific peer.
    outbound_rx: mpsc::UnboundedReceiver<BytesMut>,
}

impl<N: FlashblocksP2PNetworkHandle> FlashblocksConnection<N> {
    /// Creates a new `FlashblocksConnection` instance.
    ///
    /// # Arguments
    /// * `protocol` - The flashblocks protocol handler managing the connection.
    /// * `conn` - The underlying protocol connection for sending and receiving messages.
    /// * `peer_id` - The unique identifier of the connected peer.
    pub(crate) fn new(
        protocol: FlashblocksP2PProtocol<N>,
        conn: ProtocolConnection,
        peer_id: PeerId,
    ) -> Self {
        let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();

        protocol
            .handle
            .on_peer_connected(protocol.network.clone(), peer_id, outbound_tx);

        gauge!("flashblocks.peers", "capability" => FlashblocksP2PProtocol::<N>::capability().to_string()).increment(1);

        Self {
            protocol,
            conn,
            peer_id,
            outbound_rx,
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
            if let Poll::Ready(Some(bytes)) = this.outbound_rx.poll_recv(cx) {
                trace!(
                    target: "flashblocks::p2p",
                    peer_id = %this.peer_id,
                    "Sending serialized flashblocks protocol message to peer"
                );
                return Poll::Ready(Some(bytes));
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
                    if this.protocol.handle.handle_request_message(this.peer_id) {
                        this.protocol
                            .network
                            .reputation_change(this.peer_id, ReputationChangeKind::BadMessage);
                    }
                }
                FlashblocksP2PMsg::AcceptFlashblocks => {
                    if this.protocol.handle.handle_accept_message(this.peer_id) {
                        this.protocol
                            .network
                            .reputation_change(this.peer_id, ReputationChangeKind::BadMessage);
                    }
                }
                FlashblocksP2PMsg::RejectFlashblocks => {
                    if this.protocol.handle.handle_reject_message(this.peer_id) {
                        this.protocol
                            .network
                            .reputation_change(this.peer_id, ReputationChangeKind::BadMessage);
                    }
                }
                FlashblocksP2PMsg::CancelFlashblocks => {
                    if this.protocol.handle.handle_cancel_message(this.peer_id) {
                        this.protocol
                            .network
                            .reputation_change(this.peer_id, ReputationChangeKind::BadMessage);
                    }
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
        let authorization = &authorized_payload.authorized.authorization;
        let msg = authorized_payload.msg();
        let flashblock_timestamp = msg.metadata.flashblock_timestamp;
        let mut p2p_state = self.protocol.handle.state.lock();

        // Check if this payload is older than our current view by more than the allowed
        // grace window.
        if authorization.timestamp
            < p2p_state
                .payload_timestamp
                .saturating_sub(AUTHORIZATION_TIMESTAMP_GRACE_SEC)
        {
            tracing::warn!(
                target: "flashblocks::p2p",
                peer_id = %self.peer_id,
                current_timestamp = p2p_state.payload_timestamp,
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

        if msg.payload_id == p2p_state.payload_id
            && (msg.index as usize)
                .saturating_add(crate::protocol::handler::RECEIVE_FLASHBLOCK_GRACE_WINDOW)
                < p2p_state.flashblock_index
        {
            tracing::warn!(
                target: "flashblocks::p2p",
                peer_id = %self.peer_id,
                payload_id = %msg.payload_id,
                index = msg.index,
                current_index = p2p_state.flashblock_index,
                grace_window = crate::protocol::handler::RECEIVE_FLASHBLOCK_GRACE_WINDOW,
                "received flashblock outside receive grace window",
            );
            self.protocol
                .network
                .reputation_change(self.peer_id, ReputationChangeKind::BadMessage);
            return;
        }

        let Some(conn_state) = p2p_state.connection_state(&self.peer_id) else {
            return;
        };
        if conn_state.request_in_flight {
            tracing::warn!(
                target: "flashblocks::p2p",
                peer_id = %self.peer_id,
                payload_id = %msg.payload_id,
                index = msg.index,
                "received flashblock before request was accepted",
            );
            self.protocol
                .network
                .reputation_change(self.peer_id, ReputationChangeKind::BadMessage);
            return;
        }
        if conn_state.receive_enabled.is_none() {
            if conn_state.receive_enabled_timestamp + 2 < authorization.timestamp {
                tracing::warn!(
                    target: "flashblocks::p2p",
                    peer_id = %self.peer_id,
                    payload_id = %msg.payload_id,
                    index = msg.index,
                    "received flashblock from peer outside receive window",
                );
                self.protocol
                    .network
                    .reputation_change(self.peer_id, ReputationChangeKind::BadMessage);
            }
            return;
        }

        // Check if this peer is spamming us with the same payload index.
        if !p2p_state.note_peer_received_flashblock(authorization, msg, self.peer_id) {
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

        p2p_state.publishing_status.send_modify(|status| {
            let active_publishers = match status {
                PublishingStatus::Publishing { .. } => {
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

            if let Some((_, timestamp)) = active_publishers
                .iter_mut()
                .find(|(publisher, _)| *publisher == authorization.builder_vk)
            {
                *timestamp = authorization.timestamp;
            } else {
                active_publishers.push((authorization.builder_vk, authorization.timestamp));
            }
        });

        if let Some(flashblock_timestamp) = flashblock_timestamp {
            let now = Utc::now()
                .timestamp_nanos_opt()
                .expect("time went backwards");
            let latency = now - flashblock_timestamp;
            metrics::histogram!("flashblocks.latency").record(latency as f64 / 1_000_000_000.0);
            if let Some(score) = p2p_state
                .connection_state_mut(&self.peer_id)
                .and_then(|peer_state| peer_state.receive_enabled.as_mut())
            {
                score.record(latency);
            }
        }

        self.protocol
            .handle
            .ctx
            .publish(&mut p2p_state, authorized_payload);
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
        let state = self.protocol.handle.state.lock();

        // Check if the request is expired for dos protection.
        // It's important to ensure that this `StartPublish` request
        // is very recent, or it could be used in a replay attack.
        if state.payload_timestamp > authorization.timestamp {
            tracing::warn!(
                target: "flashblocks::p2p",
                peer_id = %self.peer_id,
                current_timestamp = state.payload_timestamp,
                timestamp = authorized_payload.authorized.authorization.timestamp,
                "received initiate build request with outdated timestamp",
            );
            drop(state);
            self.protocol
                .network
                .reputation_change(self.peer_id, ReputationChangeKind::BadMessage);
            return;
        }

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
                    state.send_to_all_peers(&p2p_msg.encode());

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
    }

    /// Handles incoming `StopPublish` messages from a peer.
    ///
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
        let state = self.protocol.handle.state.lock();

        // Check if the request is expired for dos protection.
        // It's important to ensure that this `StopPublish` request
        // is very recent, or it could be used in a replay attack.
        if state.payload_timestamp > authorization.timestamp {
            tracing::warn!(
                target: "flashblocks::p2p",
                peer_id = %self.peer_id,
                current_timestamp = state.payload_timestamp,
                timestamp = authorized_payload.authorized.authorization.timestamp,
                "Received initiate build response with outdated timestamp",
            );
            drop(state);
            self.protocol
                .network
                .reputation_change(self.peer_id, ReputationChangeKind::BadMessage);
            return;
        }

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
    }
}

/// A lightweight moving average with a configurable smoothing window.
#[derive(Clone, Debug)]
pub struct Score {
    value: Option<i64>,
    window: i64,
}

impl Score {
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
}
