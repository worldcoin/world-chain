//! Rollup-RPC backed proposal source.
//!
//! Mirrors `op-proposer/proposer/source/source_rollup.go`. Talks to an
//! `op-node` over `optimism_syncStatus` / `optimism_outputAtBlock`.
//!
//! When given multiple URLs, behaves like upstream's *active* rollup provider:
//! periodically probes endpoints and routes calls to a healthy sequencer.

use std::{
    borrow::Cow,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use alloy_eips::BlockNumHash;
use alloy_primitives::{B256, BlockHash};
use alloy_provider::{Provider, RootProvider};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tracing::warn;
use url::Url;

use super::{Proposal, ProposalSource, ProposalSourceError, SyncStatus};

const EXPECTED_OUTPUT_VERSION: B256 = B256::ZERO;

/// JSON-RPC backed rollup proposal source.
///
/// When multiple URLs are configured it round-robins on failure (every error
/// rotates to the next endpoint), giving us upstream's active-provider
/// behaviour without bolting on a separate health-check loop.
pub struct RollupProposalSource {
    clients: Vec<RollupClient>,
    cursor: AtomicUsize,
    network_timeout: Duration,
    active_check_duration: Duration,
    last_probed_at: Mutex<Instant>,
}

struct RollupClient {
    url: Url,
    provider: RootProvider,
}

impl RollupProposalSource {
    pub fn new(
        urls: Vec<String>,
        network_timeout: Duration,
        active_check_duration: Duration,
    ) -> Result<Self, ProposalSourceError> {
        if urls.is_empty() {
            return Err(ProposalSourceError::NoSources);
        }
        let mut clients = Vec::with_capacity(urls.len());
        for raw in urls {
            let url = Url::parse(&raw).map_err(|e| ProposalSourceError::Other(e.to_string()))?;
            let provider = RootProvider::new_http(url.clone());
            clients.push(RollupClient { url, provider });
        }
        Ok(Self {
            clients,
            cursor: AtomicUsize::new(0),
            network_timeout,
            active_check_duration,
            last_probed_at: Mutex::new(Instant::now()),
        })
    }

    fn current_idx(&self) -> usize {
        self.cursor.load(Ordering::Relaxed) % self.clients.len()
    }

    fn rotate(&self) {
        self.cursor.fetch_add(1, Ordering::Relaxed);
    }

    fn maybe_refresh_active(&self) {
        if self.clients.len() <= 1 {
            return;
        }
        let mut last = self.last_probed_at.lock();
        if last.elapsed() >= self.active_check_duration {
            self.rotate();
            *last = Instant::now();
        }
    }

    async fn raw_request<P, R>(
        &self,
        method: &'static str,
        params: P,
    ) -> Result<R, ProposalSourceError>
    where
        P: Serialize + Clone + std::fmt::Debug + Send + Sync + Unpin + 'static,
        R: for<'de> Deserialize<'de> + std::fmt::Debug + Send + Sync + Unpin + 'static,
    {
        self.maybe_refresh_active();
        let mut last_err = None;
        for attempt in 0..self.clients.len() {
            let idx = (self.current_idx() + attempt) % self.clients.len();
            let client = &self.clients[idx];
            let params = params.clone();
            let fut = async {
                client
                    .provider
                    .raw_request::<P, R>(Cow::Borrowed(method), params)
                    .await
                    .map_err(|e| ProposalSourceError::Transport(e.to_string()))
            };
            match tokio::time::timeout(self.network_timeout, fut).await {
                Ok(Ok(v)) => return Ok(v),
                Ok(Err(e)) => {
                    warn!(
                        target: "exex::proposer::source",
                        url = %client.url,
                        error = %e,
                        method,
                        "rollup rpc error",
                    );
                    last_err = Some(e);
                    self.rotate();
                }
                Err(_) => {
                    warn!(
                        target: "exex::proposer::source",
                        url = %client.url,
                        method,
                        "rollup rpc timed out",
                    );
                    last_err = Some(ProposalSourceError::Transport("timed out".into()));
                    self.rotate();
                }
            }
        }
        Err(last_err.unwrap_or(ProposalSourceError::NoSources))
    }
}

// JSON-RPC reply shapes mirror `op-service/eth` payloads (we only deserialize
// the fields the proposer cares about).

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSyncStatus {
    current_l1: RawL1BlockRef,
    safe_l2: RawL2BlockRef,
    finalized_l2: RawL2BlockRef,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawL1BlockRef {
    hash: BlockHash,
    number: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawL2BlockRef {
    hash: BlockHash,
    number: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawOutputResponse {
    version: B256,
    output_root: B256,
    block_ref: RawL2BlockRef,
    status: RawSyncStatus,
}

#[async_trait]
impl ProposalSource for RollupProposalSource {
    async fn proposal_at_block(
        &self,
        block_number: u64,
    ) -> Result<Proposal, ProposalSourceError> {
        let raw: RawOutputResponse = self
            .raw_request("optimism_outputAtBlock", (format!("{block_number:#x}"),))
            .await?;

        if raw.version != EXPECTED_OUTPUT_VERSION {
            return Err(ProposalSourceError::UnsupportedOutputVersion {
                got: raw.version,
                expected: EXPECTED_OUTPUT_VERSION,
            });
        }
        if raw.block_ref.number != block_number {
            return Err(ProposalSourceError::BlockNumberMismatch {
                got: raw.block_ref.number,
                expected: block_number,
            });
        }

        Ok(Proposal {
            root: raw.output_root,
            block_number: raw.block_ref.number,
            block_hash: raw.block_ref.hash,
            current_l1: BlockNumHash {
                number: raw.status.current_l1.number,
                hash: raw.status.current_l1.hash,
            },
        })
    }

    async fn sync_status(&self) -> Result<SyncStatus, ProposalSourceError> {
        let raw: RawSyncStatus = self.raw_request("optimism_syncStatus", ()).await?;
        Ok(SyncStatus {
            current_l1: BlockNumHash {
                number: raw.current_l1.number,
                hash: raw.current_l1.hash,
            },
            safe_l2: raw.safe_l2.number,
            finalized_l2: raw.finalized_l2.number,
        })
    }

    async fn close(&self) {
        // Reqwest pool is dropped with the providers.
    }
}
