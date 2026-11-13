//! Versioned wire types for the optional preconfirmed log feed.

use alloy_primitives::B256;
use alloy_rpc_types::{Filter, Log};
use alloy_rpc_types_engine::PayloadId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const API_VERSION: u8 = 1;
pub const CAPABILITY: &str = "worldchain_preconfirmed_logs_v1";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CanonicalCheckpoint {
    pub number: u64,
    pub hash: B256,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreconfirmedView {
    pub chain_id: u64,
    pub parent_hash: B256,
    pub block_number: u64,
    pub block_hash: B256,
    pub payload_id: Option<PayloadId>,
    pub flashblock_index: Option<u64>,
    pub timestamp: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreconfirmedLog {
    pub event_id: B256,
    #[serde(flatten)]
    pub log: Log,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FeedStatus {
    Initializing,
    Ready,
    Stale,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LogEventKind {
    Snapshot,
    Preconfirmed,
    Canonical,
    Removed,
    Reset,
    Progress,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResetReason {
    SourceGap,
    PayloadReplaced,
    CanonicalReorg,
    SourceUnavailable,
    Stale,
    InvalidSource,
    ResourceLimit,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreconfirmedLogEvent {
    pub sequence: u64,
    pub kind: LogEventKind,
    pub view: Option<PreconfirmedView>,
    pub logs: Vec<PreconfirmedLog>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<ResetReason>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryOptions {
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub version: u8,
    pub chain_id: u64,
    pub status: FeedStatus,
    pub view: Option<PreconfirmedView>,
    pub canonical: CanonicalCheckpoint,
    pub events: Vec<PreconfirmedLogEvent>,
    pub cursor: String,
    pub has_more: bool,
    pub view_age_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SubscriptionNotification {
    Update {
        result: Box<QueryResult>,
    },
    Error {
        code: i32,
        message: String,
        canonical: Option<CanonicalCheckpoint>,
    },
}

#[derive(Clone, Debug, Error)]
pub enum FeedError {
    #[error("invalid preconfirmed log filter: {0}")]
    InvalidFilter(&'static str),
    #[error("invalid preconfirmed log cursor: {0}")]
    InvalidCursor(&'static str),
    #[error("preconfirmed log resynchronization required: {reason}")]
    ResyncRequired {
        reason: &'static str,
        canonical: CanonicalCheckpoint,
    },
    #[error("preconfirmed log feed unavailable: {status:?}")]
    Unavailable {
        status: FeedStatus,
        canonical: CanonicalCheckpoint,
    },
    #[error("preconfirmed log resource limit exceeded: {0}")]
    LimitExceeded(&'static str),
}

impl FeedError {
    pub fn code(&self) -> i32 {
        match self {
            Self::InvalidFilter(_) | Self::InvalidCursor(_) => -32602,
            Self::Unavailable { .. } => -32050,
            Self::ResyncRequired { .. } => -32051,
            Self::LimitExceeded(_) => -32052,
        }
    }

    pub fn checkpoint(&self) -> Option<CanonicalCheckpoint> {
        match self {
            Self::ResyncRequired { canonical, .. } | Self::Unavailable { canonical, .. } => {
                Some(*canonical)
            }
            _ => None,
        }
    }
}

/// A fully validated view converted outside the feed's state lock.
#[derive(Clone, Debug)]
pub struct PreparedLogBlock {
    pub view: PreconfirmedView,
    pub logs: Vec<PreconfirmedLog>,
}

/// Limits apply to shared retained data as well as individual responses.
#[derive(Clone, Debug)]
pub struct FeedLimits {
    pub max_history_events: usize,
    pub max_history_bytes: usize,
    pub max_view_logs: usize,
    pub max_view_bytes: usize,
    pub max_snapshot_pages: usize,
    pub max_snapshot_bytes: usize,
    pub max_page_logs: usize,
    pub max_page_bytes: usize,
    pub max_filter_items: usize,
    pub stale_after: std::time::Duration,
}

impl Default for FeedLimits {
    fn default() -> Self {
        Self {
            max_history_events: 256,
            max_history_bytes: 8 * 1024 * 1024,
            max_view_logs: 32_768,
            max_view_bytes: 8 * 1024 * 1024,
            max_snapshot_pages: 8,
            max_snapshot_bytes: 16 * 1024 * 1024,
            max_page_logs: 1_000,
            max_page_bytes: 512 * 1024,
            max_filter_items: 128,
            stale_after: std::time::Duration::from_secs(5),
        }
    }
}

pub(crate) fn validate_filter(filter: &Filter, limit: usize) -> Result<(), FeedError> {
    if !matches!(
        filter.block_option,
        alloy_rpc_types::FilterBlockOption::Range {
            from_block: None,
            to_block: None
        }
    ) {
        return Err(FeedError::InvalidFilter("block bounds are not supported"));
    }
    let items = filter.address.len() + filter.topics.iter().map(|topic| topic.len()).sum::<usize>();
    if items > limit {
        return Err(FeedError::InvalidFilter(
            "too many address or topic alternatives",
        ));
    }
    Ok(())
}
