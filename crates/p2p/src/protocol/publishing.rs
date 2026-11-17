use super::{
    error::FlashblocksP2PError,
    handler::{FlashblocksHandle, FlashblocksP2PState, MAX_FRAME, Publisher, PublishingStatus},
};
use alloy_rlp::Encodable;
use alloy_rpc_types_engine::PayloadId;
use ed25519_dalek::SigningKey;
use reth_ethereum::network::api::PeerId;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use world_chain_primitives::{
    p2p::{
        Authorization, Authorized, AuthorizedMsg, AuthorizedPayload, FlashblocksP2PMsg,
        StartPublish, StopPublish,
    },
    primitives::FlashblocksPayloadV1,
};

/// Maximum wait for a previous publisher before allowing failover.
const MAX_PUBLISH_WAIT_SEC: u64 = 2;

#[derive(Clone, Copy)]
enum ControlMessage {
    Start,
    Stop,
}

impl ControlMessage {
    fn message(self) -> AuthorizedMsg {
        match self {
            Self::Start => StartPublish.into(),
            Self::Stop => StopPublish.into(),
        }
    }
}

/// A control request owns no state lock and cannot outlive its publishing intent.
#[must_use]
struct PendingControlMessage {
    deadline: Option<tokio::time::Instant>,
    authorization: Authorization,
    message: ControlMessage,
    cancelled: CancellationToken,
    payload_id: PayloadId,
    payload_timestamp: u64,
}

/// Dropping a protected payload job fences only its own publishing generation.
pub struct PublishingLease {
    handle: FlashblocksHandle,
    pub(super) scope: CancellationToken,
    pub(super) deadline: tokio::time::Instant,
}
impl Drop for PublishingLease {
    fn drop(&mut self) {
        if !matches!(self.handle.publisher, Some(Publisher::Remote(_))) {
            return;
        }
        let result = self
            .handle
            .prepare_stop_for(Some(&self.scope))
            .and_then(|pending| {
                if let Some(pending) = pending {
                    self.handle.dispatch_control(
                        pending,
                        tokio::time::Instant::now() + Duration::from_secs(3),
                    )?;
                }
                Ok(())
            });
        if let Err(error) = result {
            self.handle
                .ctx
                .metrics
                .publishing_signing_failed
                .increment(1);
            tracing::warn!(target:"flashblocks::p2p",%error,"failed to notify peers after fencing a finished payload job");
        }
    }
}

impl PendingControlMessage {
    fn new(
        state: &FlashblocksP2PState,
        authorization: Authorization,
        message: ControlMessage,
    ) -> Self {
        Self {
            deadline: None,
            authorization,
            message,
            cancelled: match message {
                ControlMessage::Start => state.pending_start.clone(),
                ControlMessage::Stop => state.pending_stop.clone(),
            },
            payload_id: state.payload_id,
            payload_timestamp: state.payload_timestamp,
        }
    }

    fn sign_local(&self, key: &SigningKey) -> Authorized {
        Authorized::new(key, self.authorization, self.message.message())
    }
}

#[cfg(test)]
mod tests;

impl FlashblocksP2PState {
    fn cancel_pending_start(&mut self) {
        self.pending_start.cancel();
        self.pending_start = CancellationToken::new();
        self.pending_start_timestamp = None;
    }

    fn new_publishing_request(&mut self) {
        self.publishing_error.send_replace(None);
        self.cancel_pending_start();
        self.pending_stop.cancel();
        self.pending_stop = CancellationToken::new();
    }
}

impl FlashblocksHandle {
    fn dispatch_control(
        &self,
        mut pending: PendingControlMessage,
        deadline: tokio::time::Instant,
    ) -> Result<(), FlashblocksP2PError> {
        if let Some(Publisher::Local(key)) = &self.publisher {
            let signed = pending.sign_local(key);
            return self.complete_control_message(pending, signed);
        }
        pending.deadline = Some(deadline);
        let permit = self
            .control_admission
            .clone()
            .try_acquire_owned()
            .map_err(|_| FlashblocksP2PError::Signing(world_chain_signer::Error::Busy));
        let permit = match permit {
            Ok(permit) => permit,
            Err(error) => {
                let state = self.state.lock();
                if matches!(pending.message, ControlMessage::Start)
                    && state.pending_start == pending.cancelled
                    && !pending.cancelled.is_cancelled()
                {
                    state.publishing_error.send_replace(Some(error.clone()));
                }
                return Err(error);
            }
        };
        let handle = self.clone();
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| {
            world_chain_signer::Error::Config("remote control signing requires an async runtime")
        })?;
        runtime.spawn(async move {
            let _permit = permit;
            let cancelled = pending.cancelled.clone();
            let is_start = matches!(pending.message, ControlMessage::Start);
            let result = tokio::select! {
                biased;
                _=cancelled.cancelled()=>Err(FlashblocksP2PError::StaleControlMessage),
                result=async {
                    if let Some(Publisher::Remote(signer))=&handle.publisher {signer.wait_until_idle(deadline).await?;}
                    handle.sign_message(pending.message.message(),pending.authorization,deadline).await
                }=>result,
            };
            let result = result.and_then(|signed| handle.complete_control_message(pending, signed));
            if let Err(error) = result {
                if !matches!(error, FlashblocksP2PError::StaleControlMessage) {
                    handle.ctx.metrics.publishing_signing_failed.increment(1);
                    tracing::warn!(target:"flashblocks::p2p",%error,"remote publishing control failed");
                }
                let state = handle.state.lock();
                if is_start && state.pending_start == cancelled && !cancelled.is_cancelled() {
                    state.publishing_error.send_replace(Some(error));
                }
            }
        });
        Ok(())
    }

    async fn sign_message(
        &self,
        message: AuthorizedMsg,
        authorization: Authorization,
        deadline: tokio::time::Instant,
    ) -> Result<Authorized, FlashblocksP2PError> {
        if authorization.builder_vk != self.publisher_key()?
            || authorization.verify(self.ctx.authorizer_vk).is_err()
        {
            return Err(FlashblocksP2PError::InvalidControlMessage);
        }
        validate_frame_lengths(message.length(), authorization.length())?;
        match self
            .publisher
            .as_ref()
            .ok_or(FlashblocksP2PError::MissingBuilderSk)?
        {
            Publisher::Local(key) => Ok(Authorized::new(key, authorization, message)),
            Publisher::Remote(signer) => {
                let raw_message = alloy_rlp::encode(&message);
                let raw_authorization = alloy_rlp::encode(authorization);
                let actor_sig = signer
                    .sign(&raw_message, &raw_authorization, deadline)
                    .await?;
                Ok(Authorized {
                    msg: message,
                    authorization,
                    actor_sig,
                })
            }
        }
    }

    /// Await remote I/O without holding the state lock, then reject a superseded session.
    pub async fn authorize_payload(
        &self,
        payload: FlashblocksPayloadV1,
        authorization: Authorization,
        deadline: tokio::time::Instant,
    ) -> Result<AuthorizedPayload<FlashblocksPayloadV1>, FlashblocksP2PError> {
        if payload.payload_id != authorization.payload_id {
            return Err(FlashblocksP2PError::ExpiredAuthorization);
        }
        let (cancelled, payload_id, timestamp) = {
            let state = self.state.lock();
            if state.payload_timestamp > authorization.timestamp
                || (state.payload_timestamp == authorization.timestamp
                    && state.payload_id != authorization.payload_id)
            {
                return Err(FlashblocksP2PError::StaleControlMessage);
            }
            if !matches!(*state.publishing_status.borrow(),PublishingStatus::Publishing {authorization:current} if current==authorization)
            {
                return Err(FlashblocksP2PError::NotClearedToPublish);
            }
            (
                state.pending_start.clone(),
                state.payload_id,
                state.payload_timestamp,
            )
        };
        let signed = tokio::select! {
            biased;
            _=cancelled.cancelled()=>return Err(FlashblocksP2PError::StaleControlMessage),
            result=self.sign_message(payload.into(),authorization,deadline)=>result?,
        };
        if tokio::time::Instant::now() >= deadline {
            return Err(world_chain_signer::Error::Deadline.into());
        }
        let state = self.state.lock();
        let payload_changed =
            state.payload_id != payload_id || state.payload_timestamp != timestamp;
        let requested_payload = state.payload_id == authorization.payload_id
            && state.payload_timestamp == authorization.timestamp;
        if cancelled.is_cancelled() || (payload_changed && !requested_payload) {
            return Err(FlashblocksP2PError::StaleControlMessage);
        }
        if !matches!(*state.publishing_status.borrow(),PublishingStatus::Publishing {authorization:current} if current==authorization)
        {
            return Err(FlashblocksP2PError::ExpiredAuthorization);
        }
        Ok(signed.into_unchecked())
    }

    /// Requests publishing clearance, signing any start notification outside the state lock.
    pub fn start_publishing(
        &self,
        authorization: Authorization,
    ) -> Result<(), FlashblocksP2PError> {
        if let Some(pending) = self.prepare_start_publishing(authorization)? {
            self.dispatch_control(
                pending,
                tokio::time::Instant::now() + Duration::from_secs(3),
            )?;
        }
        Ok(())
    }

    pub fn start_publishing_until(
        &self,
        authorization: Authorization,
        deadline: tokio::time::Instant,
    ) -> Result<PublishingLease, FlashblocksP2PError> {
        self.begin_publishing(authorization, deadline)
            .map(|scope| PublishingLease {
                handle: self.clone(),
                scope,
                deadline,
            })
    }

    fn begin_publishing(
        &self,
        authorization: Authorization,
        deadline: tokio::time::Instant,
    ) -> Result<CancellationToken, FlashblocksP2PError> {
        if tokio::time::Instant::now() >= deadline {
            return Err(world_chain_signer::Error::Deadline.into());
        }
        if let Some(pending) = self.prepare_start_publishing(authorization)? {
            let scope = pending.cancelled.clone();
            self.dispatch_control(pending, deadline)?;
            return Ok(scope);
        }
        let state = self.state.lock();
        if !matches!(*state.publishing_status.borrow(),PublishingStatus::Publishing {authorization:current}|PublishingStatus::WaitingToPublish {authorization:current,..} if current==authorization)
        {
            return Err(FlashblocksP2PError::StaleControlMessage);
        }
        Ok(state.pending_start.clone())
    }

    fn prepare_start_publishing(
        &self,
        new_authorization: Authorization,
    ) -> Result<Option<PendingControlMessage>, FlashblocksP2PError> {
        let key = self.publisher_key()?;
        if new_authorization.builder_vk != key
            || new_authorization.verify(self.ctx.authorizer_vk).is_err()
        {
            self.ctx.metrics.publishing_control_invalid.increment(1);
            return Err(FlashblocksP2PError::InvalidControlMessage);
        }

        let mut state = self.state.lock();
        if state
            .pending_start_timestamp
            .is_some_and(|timestamp| new_authorization.timestamp < timestamp)
        {
            return Ok(None);
        }
        if matches!(*state.publishing_status.borrow(), PublishingStatus::Publishing { authorization }
            if new_authorization != authorization && new_authorization.timestamp <= authorization.timestamp)
        {
            return Ok(None);
        }
        state.new_publishing_request();
        let mut send_start = false;
        state.publishing_status.send_modify(|status| match status {
            PublishingStatus::Publishing { authorization } => {
                if new_authorization.timestamp > authorization.timestamp {
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
                if new_authorization
                    .timestamp
                    .saturating_sub(most_recent_publisher)
                    >= MAX_PUBLISH_WAIT_SEC
                {
                    tracing::warn!(
                        target: "flashblocks::p2p",
                        payload_id = %new_authorization.payload_id,
                        timestamp = new_authorization.timestamp,
                        "waiting to publish timed out, starting to publish",
                    );
                    *status = PublishingStatus::Publishing {
                        authorization: new_authorization,
                    };
                } else {
                    *authorization = new_authorization;
                }
            }
            PublishingStatus::NotPublishing { .. } => send_start = true,
        });
        if send_start {
            state.pending_start_timestamp = Some(new_authorization.timestamp);
        }
        Ok(send_start
            .then(|| PendingControlMessage::new(&state, new_authorization, ControlMessage::Start)))
    }

    /// Revokes local clearance before signing the stop notification outside the state lock.
    pub fn stop_publishing(&self) -> Result<(), FlashblocksP2PError> {
        if let Some(pending) = self.prepare_stop_publishing()? {
            self.dispatch_control(
                pending,
                tokio::time::Instant::now() + Duration::from_secs(3),
            )?;
        }
        Ok(())
    }

    fn prepare_stop_publishing(
        &self,
    ) -> Result<Option<PendingControlMessage>, FlashblocksP2PError> {
        self.prepare_stop_for(None)
    }

    fn prepare_stop_for(
        &self,
        expected: Option<&CancellationToken>,
    ) -> Result<Option<PendingControlMessage>, FlashblocksP2PError> {
        self.publisher_key()?;
        let mut state = self.state.lock();
        if expected.is_some_and(|scope| *scope != state.pending_start) {
            return Ok(None);
        }
        state.cancel_pending_start();
        state
            .publishing_error
            .send_replace(Some(FlashblocksP2PError::NotClearedToPublish));
        let mut stopped = None;
        state.publishing_status.send_modify(|status| match status {
            PublishingStatus::Publishing { authorization } => {
                stopped = Some(*authorization);
                *status = PublishingStatus::NotPublishing {
                    active_publishers: Vec::new(),
                };
            }
            PublishingStatus::WaitingToPublish {
                authorization,
                active_publishers,
            } => {
                stopped = Some(*authorization);
                *status = PublishingStatus::NotPublishing {
                    active_publishers: active_publishers.clone(),
                };
            }
            PublishingStatus::NotPublishing { .. } => {}
        });
        if let Some(authorization) = stopped {
            tracing::info!(
                target: "flashblocks::p2p",
                payload_id = %authorization.payload_id,
                timestamp = authorization.timestamp,
                "stopping flashblock publishing",
            );
        }
        Ok(stopped.map(|auth| PendingControlMessage::new(&state, auth, ControlMessage::Stop)))
    }

    /// Handles an authenticated peer start without signing under the connection's state lock.
    pub(super) fn peer_start_publishing(
        &self,
        peer_id: PeerId,
        authorization: Authorization,
    ) -> Result<(), FlashblocksP2PError> {
        if let Some(pending) = self.prepare_peer_start(peer_id, authorization)? {
            self.dispatch_control(
                pending,
                tokio::time::Instant::now() + Duration::from_secs(3),
            )?;
        }
        Ok(())
    }

    fn prepare_peer_start(
        &self,
        peer_id: PeerId,
        authorization: Authorization,
    ) -> Result<Option<PendingControlMessage>, FlashblocksP2PError> {
        if !self.can_publish() {
            return Ok(None);
        }
        let mut state = self.state.lock();
        let local_timestamp = match &*state.publishing_status.borrow() {
            PublishingStatus::Publishing { authorization }
            | PublishingStatus::WaitingToPublish { authorization, .. } => authorization.timestamp,
            PublishingStatus::NotPublishing { .. } => 0,
        };
        let current_timestamp = state
            .payload_timestamp
            .max(local_timestamp)
            .max(state.pending_start_timestamp.unwrap_or_default());
        if current_timestamp > authorization.timestamp {
            tracing::warn!(
                target: "flashblocks::p2p",
                %peer_id,
                current_timestamp,
                timestamp = authorization.timestamp,
                "received StartPublish with outdated timestamp",
            );
            return Err(FlashblocksP2PError::ExpiredAuthorization);
        }
        state.cancel_pending_start();
        state
            .publishing_error
            .send_replace(Some(FlashblocksP2PError::NotClearedToPublish));
        let mut stopped = None;
        state.publishing_status.send_modify(|status| {
            let active_publishers = match status {
                PublishingStatus::Publishing {
                    authorization: ours,
                } => {
                    stopped = Some(*ours);
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
                    tracing::warn!(
                        target: "flashblocks::p2p",
                        %peer_id,
                        "received StartPublish while already waiting to publish",
                    );
                    active_publishers
                }
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
        Ok(stopped.map(|auth| PendingControlMessage::new(&state, auth, ControlMessage::Stop)))
    }

    fn complete_control_message(
        &self,
        pending: PendingControlMessage,
        signed: Authorized,
    ) -> Result<(), FlashblocksP2PError> {
        if signed.authorization != pending.authorization
            || signed.msg != pending.message.message()
            || signed.authorization.builder_vk != self.publisher_key()?
            || signed.verify(self.ctx.authorizer_vk).is_err()
        {
            self.ctx.metrics.publishing_control_invalid.increment(1);
            return Err(FlashblocksP2PError::InvalidControlMessage);
        }
        let bytes = FlashblocksP2PMsg::Authorized(signed).encode();
        let mut state = self.state.lock();
        if pending
            .deadline
            .is_some_and(|until| tokio::time::Instant::now() >= until)
        {
            return Err(world_chain_signer::Error::Deadline.into());
        }
        let payload_changed = state.payload_id != pending.payload_id
            || state.payload_timestamp != pending.payload_timestamp;
        let requested_payload = state.payload_id == pending.authorization.payload_id
            && state.payload_timestamp == pending.authorization.timestamp;
        if pending.cancelled.is_cancelled()
            || (payload_changed && !requested_payload)
            || !matches!(
                *state.publishing_status.borrow(),
                PublishingStatus::NotPublishing { .. }
            )
        {
            self.ctx.metrics.publishing_control_stale.increment(1);
            return Err(FlashblocksP2PError::StaleControlMessage);
        }
        state.send_to_all_peers(&bytes);
        if matches!(pending.message, ControlMessage::Start) {
            state.publishing_status.send_modify(|status| {
                if let PublishingStatus::NotPublishing { active_publishers } = status {
                    if active_publishers.is_empty() {
                        tracing::info!(
                            target: "flashblocks::p2p",
                            payload_id = %pending.authorization.payload_id,
                            "starting to publish flashblocks",
                        );
                        *status = PublishingStatus::Publishing {
                            authorization: pending.authorization,
                        };
                    } else {
                        tracing::info!(
                            target: "flashblocks::p2p",
                            payload_id = %pending.authorization.payload_id,
                            "waiting to publish flashblocks",
                        );
                        *status = PublishingStatus::WaitingToPublish {
                            authorization: pending.authorization,
                            active_publishers: active_publishers.clone(),
                        };
                    }
                }
            });
            state.pending_start_timestamp = None;
        }
        Ok(())
    }
}

fn validate_frame_lengths(message: usize, authorization: usize) -> Result<(), FlashblocksP2PError> {
    let payload = message
        .checked_add(authorization)
        .and_then(|n| n.checked_add(66))
        .ok_or(FlashblocksP2PError::FrameTooLarge)?;
    let header = alloy_rlp::Header {
        list: true,
        payload_length: payload,
    }
    .length();
    let frame = payload
        .checked_add(header)
        .and_then(|n| n.checked_add(1))
        .ok_or(FlashblocksP2PError::FrameTooLarge)?;
    if frame > MAX_FRAME {
        return Err(FlashblocksP2PError::FrameTooLarge);
    }
    Ok(())
}
