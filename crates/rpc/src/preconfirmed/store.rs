//! Bounded node-local history shared by polling and WebSocket consumers.

use std::{
    collections::{HashSet, VecDeque},
    sync::Arc,
    time::Instant,
};

use alloy_primitives::{B256, keccak256};
use alloy_rpc_types::Filter;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::{Semaphore, watch};
use tracing::warn;

use super::types::*;

const MAX_CURSOR_BYTES: usize = 1_024;
const MIN_RETAINED_BYTES: usize = 2_048;

#[derive(Clone, Debug)]
pub struct PreconfirmedLogStore {
    inner: Arc<Mutex<State>>,
    limits: Arc<FeedLimits>,
    updates: watch::Sender<u64>,
    queries: Arc<Semaphore>,
}

#[derive(Debug)]
struct State {
    chain_id: u64,
    epoch: B256,
    sequence: u64,
    canonical: CanonicalCheckpoint,
    current: Option<Arc<PreparedLogBlock>>,
    last_validated: Option<Instant>,
    unavailable: bool,
    history: VecDeque<Arc<StoredEvent>>,
    history_bytes: usize,
    snapshots: VecDeque<Arc<Snapshot>>,
    snapshot_bytes: usize,
    next_snapshot: u64,
    last_status: FeedStatus,
}

#[derive(Debug)]
struct StoredEvent {
    event: PreconfirmedLogEvent,
    bytes: usize,
}

#[derive(Clone, Debug)]
struct Snapshot {
    id: u64,
    sequence: u64,
    filter_hash: B256,
    view: Option<PreconfirmedView>,
    canonical: CanonicalCheckpoint,
    validated_at: Option<Instant>,
    logs: Vec<PreconfirmedLog>,
    bytes: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    epoch: B256,
    filter: B256,
    sequence: u64,
    offset: usize,
    snapshot: Option<u64>,
}

impl Cursor {
    fn encode(&self) -> Result<String, FeedError> {
        serde_json::to_vec(self)
            .map(hex::encode)
            .map_err(|_| FeedError::InvalidCursor("could not encode cursor"))
    }

    fn decode(value: &str) -> Result<Self, FeedError> {
        if value.len() > MAX_CURSOR_BYTES || !value.len().is_multiple_of(2) {
            return Err(FeedError::InvalidCursor("invalid cursor length"));
        }
        let bytes = hex::decode(value).map_err(|_| FeedError::InvalidCursor("invalid encoding"))?;
        serde_json::from_slice(&bytes)
            .map_err(|_| FeedError::InvalidCursor("invalid cursor schema"))
    }
}

impl PreconfirmedLogStore {
    pub fn new(
        chain_id: u64,
        canonical: CanonicalCheckpoint,
        limits: FeedLimits,
    ) -> Result<Self, FeedError> {
        if limits.max_history_events == 0
            || limits.max_history_bytes < MIN_RETAINED_BYTES
            || limits.max_view_logs == 0
            || limits.max_view_bytes < MIN_RETAINED_BYTES
            || limits.max_snapshot_pages == 0
            || limits.max_snapshot_bytes < limits.max_view_bytes
            || limits.max_page_logs == 0
            || limits.max_page_bytes <= MIN_RETAINED_BYTES
            || limits.max_filter_items == 0
            || limits.stale_after.is_zero()
        {
            return Err(FeedError::LimitExceeded("invalid feed limits"));
        }
        let (updates, _) = watch::channel(0);
        Ok(Self {
            inner: Arc::new(Mutex::new(State {
                chain_id,
                epoch: B256::from(rand::random::<[u8; 32]>()),
                sequence: 0,
                canonical,
                current: None,
                last_validated: None,
                unavailable: false,
                history: VecDeque::new(),
                history_bytes: 0,
                snapshots: VecDeque::new(),
                snapshot_bytes: 0,
                next_snapshot: 0,
                last_status: FeedStatus::Initializing,
            })),
            limits: Arc::new(limits),
            updates,
            queries: Arc::new(Semaphore::new(8)),
        })
    }

    pub fn update_receiver(&self) -> watch::Receiver<u64> {
        self.updates.subscribe()
    }

    pub fn checkpoint(&self) -> CanonicalCheckpoint {
        self.inner.lock().canonical
    }

    pub fn limits(&self) -> &FeedLimits {
        &self.limits
    }

    pub fn apply_validated(
        &self,
        block: PreparedLogBlock,
        validated_at: Instant,
    ) -> Result<(), FeedError> {
        let bytes = serialized_size(&block.logs)?;
        if block.logs.len() > self.limits.max_view_logs || bytes > self.limits.max_view_bytes {
            self.mark_unavailable(ResetReason::ResourceLimit);
            return Err(FeedError::LimitExceeded("validated view is too large"));
        }
        self.update(|state| {
            if block.view.chain_id != state.chain_id
                || block.view.parent_hash != state.canonical.hash
                || state.canonical.number.checked_add(1) != Some(block.view.block_number)
                || block.view.payload_id.is_none()
                || block.view.flashblock_index.is_none()
            {
                return Err(FeedError::ResyncRequired {
                    reason: "validated payload does not extend the canonical checkpoint",
                    canonical: state.canonical,
                });
            }
            let mut start = 0;
            if let Some(previous) = state.current.clone() {
                let same_payload = previous.view.payload_id == block.view.payload_id
                    && previous.view.parent_hash == block.view.parent_hash;
                if same_payload && previous.view.flashblock_index == block.view.flashblock_index {
                    if previous.view.block_hash == block.view.block_hash {
                        return Ok(());
                    }
                    self.reset_locked(state, ResetReason::InvalidSource)?;
                    return Err(FeedError::ResyncRequired {
                        reason: "conflicting validated payload index",
                        canonical: state.canonical,
                    });
                }
                if same_payload && block.view.flashblock_index < previous.view.flashblock_index {
                    return Ok(());
                }
                let append_only = same_payload
                    && block.logs.len() >= previous.logs.len()
                    && previous
                        .logs
                        .iter()
                        .zip(&block.logs)
                        .all(|(a, b)| a.event_id == b.event_id);
                if append_only {
                    if previous
                        .view
                        .flashblock_index
                        .and_then(|index| index.checked_add(1))
                        != block.view.flashblock_index
                    {
                        self.reset_locked(state, ResetReason::SourceGap)?;
                    } else {
                        start = previous.logs.len();
                    }
                } else {
                    self.append_locked(
                        state,
                        LogEventKind::Removed,
                        Some(previous.view.clone()),
                        removed_logs(&previous.logs),
                        Some(ResetReason::PayloadReplaced),
                    )?;
                }
            }
            let kind = if block.logs.len() == start {
                LogEventKind::Progress
            } else {
                LogEventKind::Preconfirmed
            };
            self.append_locked(
                state,
                kind,
                Some(block.view.clone()),
                block.logs[start..].to_vec(),
                None,
            )?;
            state.current = Some(Arc::new(block));
            state.last_validated = Some(validated_at);
            state.unavailable = false;
            state.last_status = status(state, &self.limits, Instant::now());
            metrics::counter!("worldchain.preconfirmed.validated_views").increment(1);
            Ok(())
        })
    }

    pub fn apply_canonical(
        &self,
        reverted: Vec<PreparedLogBlock>,
        committed: Vec<PreparedLogBlock>,
        checkpoint: CanonicalCheckpoint,
    ) -> Result<(), FeedError> {
        for block in reverted.iter().chain(&committed) {
            if block.logs.len() > self.limits.max_view_logs
                || serialized_size(&block.logs)? > self.limits.max_view_bytes
            {
                self.reset_to(checkpoint, ResetReason::ResourceLimit);
                return Err(FeedError::LimitExceeded("canonical update is too large"));
            }
        }
        self.update(|state| {
            if reverted.is_empty() && committed.is_empty() && checkpoint == state.canonical {
                return Ok(());
            }
            if reverted
                .iter()
                .chain(&committed)
                .any(|block| block.view.chain_id != state.chain_id)
            {
                return Err(resync(state, "canonical update belongs to another chain"));
            }
            state.canonical = checkpoint;
            if let Some(current) = state.current.take() {
                let accepted = committed
                    .iter()
                    .find(|block| block.view.block_number == current.view.block_number)
                    .map(|block| {
                        block
                            .logs
                            .iter()
                            .map(|log| log.event_id)
                            .collect::<HashSet<_>>()
                    })
                    .unwrap_or_default();
                let removed = current
                    .logs
                    .iter()
                    .filter(|log| !reverted.is_empty() || !accepted.contains(&log.event_id))
                    .cloned()
                    .collect::<Vec<_>>();
                if !removed.is_empty() {
                    self.append_locked(
                        state,
                        LogEventKind::Removed,
                        Some(current.view.clone()),
                        removed_logs(&removed),
                        Some(if reverted.is_empty() {
                            ResetReason::PayloadReplaced
                        } else {
                            ResetReason::CanonicalReorg
                        }),
                    )?;
                }
            }
            for block in reverted {
                self.append_locked(
                    state,
                    LogEventKind::Removed,
                    Some(block.view),
                    removed_logs(&block.logs),
                    Some(ResetReason::CanonicalReorg),
                )?;
            }
            for block in committed {
                self.append_locked(
                    state,
                    LogEventKind::Canonical,
                    Some(block.view),
                    block.logs,
                    None,
                )?;
            }
            self.append_locked(state, LogEventKind::Progress, None, Vec::new(), None)
        })
    }

    pub fn reset_to(&self, checkpoint: CanonicalCheckpoint, reason: ResetReason) {
        self.update(|state| {
            state.canonical = checkpoint;
            if let Err(error) = self.reset_locked(state, reason) {
                warn!(target: "rpc::preconfirmed", %error, "failed to retain preconfirmed reset");
            }
        });
    }

    pub fn mark_unavailable(&self, reason: ResetReason) {
        self.update(|state| {
            if let Err(error) = self.reset_locked(state, reason) {
                warn!(target: "rpc::preconfirmed", %error, "failed to retain preconfirmed unavailability");
            }
            state.unavailable = true;
            state.last_status = FeedStatus::Unavailable;
        });
    }

    pub fn tick(&self) {
        self.update(|state| {
            if let Some(validated_at) = state.last_validated {
                metrics::gauge!("worldchain.preconfirmed.view_age_seconds")
                    .set(Instant::now().saturating_duration_since(validated_at).as_secs_f64());
            }
            let current_status = status(state, &self.limits, Instant::now());
            if current_status != state.last_status {
                state.last_status = current_status;
                if let Err(error) = self.append_locked(state, LogEventKind::Progress, None, Vec::new(), None) {
                    warn!(target: "rpc::preconfirmed", %error, "failed to retain preconfirmed status change");
                }
            }
        });
    }

    pub fn query(&self, filter: Filter, options: QueryOptions) -> Result<QueryResult, FeedError> {
        let started = Instant::now();
        let result = self.query_inner(filter, options);
        metrics::histogram!("worldchain.preconfirmed.query_seconds")
            .record(started.elapsed().as_secs_f64());
        metrics::counter!("worldchain.preconfirmed.queries", "result" => if result.is_ok() { "success" } else { "error" })
            .increment(1);
        if let Err(error) = &result {
            let reason = match error {
                FeedError::InvalidFilter(_) | FeedError::InvalidCursor(_) => "invalid_params",
                FeedError::ResyncRequired { .. } => "resync",
                FeedError::Unavailable { .. } => "unavailable",
                FeedError::LimitExceeded(_) => "limit",
            };
            metrics::counter!("worldchain.preconfirmed.query_errors", "reason" => reason)
                .increment(1);
            if let FeedError::ResyncRequired { reason, .. } = error {
                let expired = match *reason {
                    "event history expired" | "event history unavailable" => Some("history"),
                    "snapshot pagination expired" => Some("snapshot"),
                    _ => None,
                };
                if let Some(kind) = expired {
                    metrics::counter!("worldchain.preconfirmed.replay_expiry", "kind" => kind)
                        .increment(1);
                }
            }
        }
        result
    }

    fn query_inner(&self, filter: Filter, options: QueryOptions) -> Result<QueryResult, FeedError> {
        let _permit = self
            .queries
            .clone()
            .try_acquire_owned()
            .map_err(|_| FeedError::LimitExceeded("too many concurrent queries"))?;
        validate_filter(&filter, self.limits.max_filter_items)?;
        let limit = options.limit.unwrap_or(self.limits.max_page_logs);
        if limit == 0 || limit > self.limits.max_page_logs {
            return Err(FeedError::LimitExceeded("invalid page limit"));
        }
        let filter_hash = filter_hash(&filter);
        let cursor = options.cursor.as_deref().map(Cursor::decode).transpose()?;
        let (
            chain_id,
            epoch,
            sequence,
            canonical,
            current,
            last_validated,
            current_status,
            history,
            snapshot,
        ) = {
            let state = self.inner.lock();
            let current_status = status(&state, &self.limits, Instant::now());
            if let Some(cursor) = &cursor {
                if cursor.version != API_VERSION || cursor.epoch != state.epoch {
                    return Err(resync(
                        &state,
                        "cursor belongs to another node or generation",
                    ));
                }
                if cursor.filter != filter_hash {
                    return Err(FeedError::InvalidCursor("cursor belongs to another filter"));
                }
                if cursor.sequence > state.sequence {
                    return Err(FeedError::InvalidCursor("cursor is ahead of the feed"));
                }
            }
            if matches!(
                current_status,
                FeedStatus::Initializing | FeedStatus::Unavailable | FeedStatus::Stale
            ) {
                return Err(FeedError::Unavailable {
                    status: current_status,
                    canonical: state.canonical,
                });
            }
            let snapshot = if let Some(id) = cursor.as_ref().and_then(|cursor| cursor.snapshot) {
                Some(
                    state
                        .snapshots
                        .iter()
                        .find(|snapshot| snapshot.id == id)
                        .cloned()
                        .ok_or_else(|| resync(&state, "snapshot pagination expired"))?,
                )
            } else {
                None
            };
            (
                state.chain_id,
                state.epoch,
                state.sequence,
                state.canonical,
                state.current.clone(),
                state.last_validated,
                current_status,
                state.history.iter().cloned().collect::<Vec<_>>(),
                snapshot,
            )
        };
        let mut next = Cursor {
            version: API_VERSION,
            epoch,
            filter: filter_hash,
            sequence,
            offset: 0,
            snapshot: None,
        };
        let mut response = QueryResult {
            version: API_VERSION,
            chain_id,
            status: current_status,
            view: current.as_ref().map(|block| block.view.clone()),
            canonical,
            events: Vec::new(),
            cursor: String::new(),
            has_more: false,
            view_age_ms: age_ms(last_validated),
        };
        if cursor.is_none() || snapshot.is_some() {
            let is_new = snapshot.is_none();
            let mut snapshot = if let Some(snapshot) = snapshot {
                let cursor = cursor
                    .as_ref()
                    .ok_or(FeedError::InvalidCursor("missing snapshot cursor"))?;
                if snapshot.sequence != cursor.sequence || snapshot.filter_hash != filter_hash {
                    return Err(FeedError::InvalidCursor("snapshot scope does not match"));
                }
                snapshot
            } else {
                let logs = current
                    .as_ref()
                    .map(|block| matching(&block.logs, &filter))
                    .unwrap_or_default();
                let bytes = serialized_size(&logs)?;
                Arc::new(Snapshot {
                    id: 0,
                    sequence,
                    filter_hash,
                    view: response.view.clone(),
                    canonical,
                    validated_at: last_validated,
                    logs,
                    bytes,
                })
            };
            let offset = cursor
                .as_ref()
                .map(|cursor| cursor.offset)
                .unwrap_or_default();
            if offset > snapshot.logs.len() || (!is_new && offset == snapshot.logs.len()) {
                return Err(FeedError::InvalidCursor("snapshot offset is out of range"));
            }
            next.sequence = snapshot.sequence;
            response.view = snapshot.view.clone();
            response.canonical = snapshot.canonical;
            response.view_age_ms = age_ms(snapshot.validated_at);
            if let Some(validated_at) = snapshot.validated_at {
                metrics::gauge!("worldchain.preconfirmed.snapshot_age_seconds").set(
                    Instant::now()
                        .saturating_duration_since(validated_at)
                        .as_secs_f64(),
                );
            }
            let mut event = PreconfirmedLogEvent {
                sequence: snapshot.sequence,
                kind: LogEventKind::Snapshot,
                view: snapshot.view.clone(),
                logs: Vec::new(),
                reason: None,
            };
            let envelope_bytes = serialized_size(&response)?;
            let event_bytes = serialized_size(&event)?;
            let complete_cursor_bytes = next.encode()?.len();
            let full_bytes = envelope_bytes
                .saturating_add(event_bytes)
                .saturating_add(snapshot.bytes.saturating_sub(2))
                .saturating_add(complete_cursor_bytes);
            if is_new && (snapshot.logs.len() > limit || full_bytes > self.limits.max_page_bytes) {
                let mut state = self.inner.lock();
                if state.epoch != epoch {
                    return Err(resync(&state, "feed generation changed during snapshot"));
                }
                state.next_snapshot = state
                    .next_snapshot
                    .checked_add(1)
                    .ok_or(FeedError::LimitExceeded("snapshot identifier exhausted"))?;
                Arc::make_mut(&mut snapshot).id = state.next_snapshot;
            }
            let mut partial = next.clone();
            partial.snapshot = Some(snapshot.id);
            partial.offset = snapshot.logs.len().saturating_sub(1);
            let cursor_bytes = if is_new && snapshot.id == 0 {
                complete_cursor_bytes
            } else {
                complete_cursor_bytes.max(partial.encode()?.len())
            };
            let reserve = envelope_bytes
                .saturating_add(event_bytes)
                .saturating_add(cursor_bytes);
            let budget =
                self.limits
                    .max_page_bytes
                    .checked_sub(reserve)
                    .ok_or(FeedError::LimitExceeded(
                        "snapshot metadata exceeds the response byte limit",
                    ))?;
            let (logs, consumed) = page_logs(&snapshot.logs[offset..], limit, budget)?.ok_or(
                FeedError::LimitExceeded("one log exceeds the response byte limit"),
            )?;
            event.logs = logs;
            response.events.push(event);
            if offset + consumed < snapshot.logs.len() {
                next.snapshot = Some(snapshot.id);
                next.offset = offset + consumed;
                response.has_more = true;
                if is_new {
                    let mut state = self.inner.lock();
                    if state.epoch != epoch {
                        return Err(resync(&state, "feed generation changed during snapshot"));
                    }
                    while !state.snapshots.is_empty()
                        && (state.snapshots.len() >= self.limits.max_snapshot_pages
                            || state.snapshot_bytes.saturating_add(snapshot.bytes)
                                > self.limits.max_snapshot_bytes)
                    {
                        if let Some(evicted) = state.snapshots.pop_front() {
                            state.snapshot_bytes -= evicted.bytes;
                        }
                    }
                    if snapshot.bytes > self.limits.max_snapshot_bytes {
                        return Err(FeedError::LimitExceeded("snapshot is too large"));
                    }
                    state.snapshot_bytes += snapshot.bytes;
                    state.snapshots.push_back(snapshot.clone());
                }
            } else {
                response.has_more = sequence > snapshot.sequence;
            }
        } else if let Some(cursor) = cursor {
            if let Some(first) = history.first() {
                if cursor.sequence < first.event.sequence.saturating_sub(1) {
                    return Err(FeedError::ResyncRequired {
                        reason: "event history expired",
                        canonical,
                    });
                }
            } else if cursor.sequence != sequence {
                return Err(FeedError::ResyncRequired {
                    reason: "event history unavailable",
                    canonical,
                });
            }
            next.sequence = cursor.sequence;
            let mut remaining = limit;
            let envelope_bytes = serialized_size(&response)?;
            let mut event_bytes = 0usize;
            let mut first = true;
            for stored in history
                .iter()
                .filter(|event| event.event.sequence > cursor.sequence)
            {
                let logs = matching(&stored.event.logs, &filter);
                let offset = if first { cursor.offset } else { 0 };
                first = false;
                if offset > logs.len() || (offset != 0 && offset == logs.len()) {
                    return Err(FeedError::InvalidCursor("event offset is out of range"));
                }
                if remaining == 0 {
                    response.has_more = true;
                    break;
                }
                let mut event = PreconfirmedLogEvent {
                    sequence: stored.event.sequence,
                    kind: stored.event.kind,
                    view: stored.event.view.clone(),
                    logs: Vec::new(),
                    reason: stored.event.reason,
                };
                let complete = Cursor {
                    sequence: event.sequence,
                    offset: 0,
                    ..next.clone()
                };
                let partial = Cursor {
                    sequence: event.sequence - 1,
                    offset: logs.len().saturating_sub(1),
                    ..next.clone()
                };
                let reserve = envelope_bytes
                    .saturating_add(event_bytes)
                    .saturating_add(usize::from(!response.events.is_empty()))
                    .saturating_add(serialized_size(&event)?)
                    .saturating_add(complete.encode()?.len().max(partial.encode()?.len()));
                let page = match self.limits.max_page_bytes.checked_sub(reserve) {
                    Some(budget) => page_logs(&logs[offset..], remaining, budget)?,
                    None => None,
                };
                let (page, consumed) = match page {
                    Some(page) => page,
                    None if !response.events.is_empty() => {
                        response.has_more = true;
                        break;
                    }
                    None => {
                        return Err(FeedError::LimitExceeded(
                            "one event or log exceeds the response byte limit",
                        ));
                    }
                };
                remaining -= consumed.max(1);
                event.logs = page;
                event_bytes += serialized_size(&event)? + usize::from(!response.events.is_empty());
                response.events.push(event);
                if offset + consumed < logs.len() {
                    next.sequence = stored.event.sequence - 1;
                    next.offset = offset + consumed;
                    response.has_more = true;
                    break;
                }
                next.sequence = stored.event.sequence;
                next.offset = 0;
            }
            if cursor.offset != 0 && first {
                return Err(FeedError::InvalidCursor(
                    "event offset has no matching event",
                ));
            }
            response.has_more |= next.sequence < sequence;
        }
        response.cursor = next.encode()?;
        if serialized_size(&response)? > self.limits.max_page_bytes {
            return Err(FeedError::LimitExceeded("response exceeds the byte limit"));
        }
        Ok(response)
    }

    fn update<T>(&self, mutate: impl FnOnce(&mut State) -> T) -> T {
        let (result, changed) = {
            let mut state = self.inner.lock();
            let before = (state.epoch, state.sequence, state.unavailable);
            let result = mutate(&mut state);
            let changed = (before != (state.epoch, state.sequence, state.unavailable))
                .then_some(state.sequence);
            (result, changed)
        };
        if let Some(sequence) = changed {
            self.updates.send_replace(sequence);
        }
        result
    }

    fn reset_locked(&self, state: &mut State, reason: ResetReason) -> Result<(), FeedError> {
        state.current = None;
        state.unavailable = true;
        state.last_status = FeedStatus::Unavailable;
        self.append_locked(state, LogEventKind::Reset, None, Vec::new(), Some(reason))?;
        metrics::counter!("worldchain.preconfirmed.resets", "reason" => format!("{reason:?}"))
            .increment(1);
        Ok(())
    }

    fn append_locked(
        &self,
        state: &mut State,
        kind: LogEventKind,
        view: Option<PreconfirmedView>,
        logs: Vec<PreconfirmedLog>,
        reason: Option<ResetReason>,
    ) -> Result<(), FeedError> {
        let Some(sequence) = state.sequence.checked_add(1) else {
            invalidate(state);
            return Err(FeedError::LimitExceeded("event sequence exhausted"));
        };
        let event = PreconfirmedLogEvent {
            sequence,
            kind,
            view,
            logs,
            reason,
        };
        let bytes = match serialized_size(&event) {
            Ok(bytes) => bytes,
            Err(error) => {
                invalidate(state);
                metrics::counter!("worldchain.preconfirmed.serialization_errors").increment(1);
                return Err(error);
            }
        };
        if bytes > self.limits.max_history_bytes {
            invalidate(state);
            return Err(FeedError::LimitExceeded(
                "one event exceeds the retained history byte limit",
            ));
        }
        state.sequence = sequence;
        while !state.history.is_empty()
            && (state.history.len() >= self.limits.max_history_events
                || state.history_bytes.saturating_add(bytes) > self.limits.max_history_bytes)
        {
            if let Some(evicted) = state.history.pop_front() {
                state.history_bytes = state.history_bytes.saturating_sub(evicted.bytes);
            }
        }
        state.history_bytes += bytes;
        state
            .history
            .push_back(Arc::new(StoredEvent { event, bytes }));
        metrics::gauge!("worldchain.preconfirmed.history_bytes").set(state.history_bytes as f64);
        Ok(())
    }
}

fn invalidate(state: &mut State) {
    state.epoch = B256::from(rand::random::<[u8; 32]>());
    state.sequence = 0;
    state.history.clear();
    state.history_bytes = 0;
    state.snapshots.clear();
    state.snapshot_bytes = 0;
    state.current = None;
    state.unavailable = true;
    state.last_status = FeedStatus::Unavailable;
    metrics::gauge!("worldchain.preconfirmed.history_bytes").set(0.0);
}

fn status(state: &State, limits: &FeedLimits, now: Instant) -> FeedStatus {
    if state.unavailable {
        FeedStatus::Unavailable
    } else if let Some(validated_at) = state.last_validated {
        if now.saturating_duration_since(validated_at) > limits.stale_after {
            FeedStatus::Stale
        } else {
            FeedStatus::Ready
        }
    } else {
        FeedStatus::Initializing
    }
}

fn age_ms(validated_at: Option<Instant>) -> Option<u64> {
    validated_at.map(|at| {
        Instant::now()
            .saturating_duration_since(at)
            .as_millis()
            .min(u64::MAX as u128) as u64
    })
}

fn resync(state: &State, reason: &'static str) -> FeedError {
    FeedError::ResyncRequired {
        reason,
        canonical: state.canonical,
    }
}

fn serialized_size(value: &impl Serialize) -> Result<usize, FeedError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|_| FeedError::LimitExceeded("could not serialize feed data"))
}

fn matching(logs: &[PreconfirmedLog], filter: &Filter) -> Vec<PreconfirmedLog> {
    logs.iter()
        .filter(|log| filter.matches(&log.log.inner))
        .cloned()
        .collect()
}

fn removed_logs(logs: &[PreconfirmedLog]) -> Vec<PreconfirmedLog> {
    logs.iter()
        .cloned()
        .map(|mut log| {
            log.log.removed = true;
            log
        })
        .collect()
}

fn page_logs(
    logs: &[PreconfirmedLog],
    limit: usize,
    max_bytes: usize,
) -> Result<Option<(Vec<PreconfirmedLog>, usize)>, FeedError> {
    let mut bytes = 0usize;
    let mut page = Vec::new();
    for log in logs.iter().take(limit) {
        let size = serialized_size(log)?.saturating_add(usize::from(!page.is_empty()));
        if bytes.saturating_add(size) > max_bytes {
            if page.is_empty() {
                return Ok(None);
            }
            break;
        }
        bytes += size;
        page.push(log.clone());
    }
    let count = page.len();
    Ok(Some((page, count)))
}

fn filter_hash(filter: &Filter) -> B256 {
    let mut bytes = b"worldchain-preconfirmed-filter-v1".to_vec();
    let mut addresses = filter.address.iter().copied().collect::<Vec<_>>();
    addresses.sort_unstable();
    bytes.extend_from_slice(&(addresses.len() as u64).to_be_bytes());
    for address in addresses {
        bytes.extend_from_slice(address.as_slice());
    }
    for topic in &filter.topics {
        let mut alternatives = topic.iter().copied().collect::<Vec<_>>();
        alternatives.sort_unstable();
        bytes.extend_from_slice(&(alternatives.len() as u64).to_be_bytes());
        for alternative in alternatives {
            bytes.extend_from_slice(alternative.as_slice());
        }
    }
    keccak256(bytes)
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
