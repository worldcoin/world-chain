//! Supervisor RPC backed proposal source (interop).
//!
//! Mirrors `op-proposer/proposer/source/source_supervisor.go`. Calls
//! `supervisor_syncStatus` and `supervisor_superRootAtTimestamp`. Multiple
//! clients are queried in parallel for `SyncStatus` (we take the earliest
//! `MinSyncedL1`) and sequentially for proposals (failover).
//!
//! Super-root versioning: only v1 is supported, matching upstream.

use std::{borrow::Cow, time::Duration};

use alloy_primitives::B256;
use alloy_provider::{Provider, RootProvider};
use async_trait::async_trait;
use futures::future::join_all;
use serde::Deserialize;
use tracing::warn;
use url::Url;

use super::{
    L1BlockRef, LegacyProposalData, Proposal, ProposalSource, ProposalSourceError, SyncStatus,
};

const SUPER_ROOT_VERSION_V1: u8 = 1;

pub struct SupervisorProposalSource {
    clients: Vec<SupervisorClient>,
    network_timeout: Duration,
}

struct SupervisorClient {
    url: Url,
    provider: RootProvider,
}

impl SupervisorProposalSource {
    pub fn new(urls: Vec<String>, network_timeout: Duration) -> Result<Self, ProposalSourceError> {
        if urls.is_empty() {
            return Err(ProposalSourceError::NoSources);
        }
        let mut clients = Vec::with_capacity(urls.len());
        for raw in urls {
            let url = Url::parse(&raw).map_err(|e| ProposalSourceError::Other(e.to_string()))?;
            let provider = RootProvider::new_http(url.clone());
            clients.push(SupervisorClient { url, provider });
        }
        Ok(Self {
            clients,
            network_timeout,
        })
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RawSupervisorSyncStatus {
    min_synced_l1: RawL1BlockRef,
    safe_timestamp: u64,
    finalized_timestamp: u64,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RawL1BlockRef {
    hash: alloy_primitives::BlockHash,
    number: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSuperRootResponse {
    timestamp: u64,
    super_root: B256,
    cross_safe_derived_from: RawL1BlockRef,
    version: u8,
    super_root_bytes: alloy_primitives::Bytes,
}

impl From<RawL1BlockRef> for L1BlockRef {
    fn from(r: RawL1BlockRef) -> Self {
        Self {
            number: r.number,
            hash: r.hash,
        }
    }
}

#[async_trait]
impl ProposalSource for SupervisorProposalSource {
    async fn proposal_at_sequence_num(
        &self,
        timestamp: u64,
    ) -> Result<Proposal, ProposalSourceError> {
        let mut errs: Vec<String> = Vec::new();
        for client in &self.clients {
            let fut = client.provider.raw_request::<_, RawSuperRootResponse>(
                Cow::Borrowed("supervisor_superRootAtTimestamp"),
                (format!("{timestamp:#x}"),),
            );
            match tokio::time::timeout(self.network_timeout, fut).await {
                Ok(Ok(r)) => {
                    if r.version != SUPER_ROOT_VERSION_V1 {
                        let msg = format!(
                            "unsupported super root version {} from supervisor {}",
                            r.version, client.url
                        );
                        warn!(target: "exex::proposer::source", error = %msg);
                        errs.push(msg);
                        continue;
                    }
                    return Ok(Proposal {
                        root: r.super_root,
                        sequence_num: r.timestamp,
                        super_root_marshalled: Some(r.super_root_bytes.to_vec()),
                        current_l1: r.cross_safe_derived_from.into(),
                        legacy: LegacyProposalData::default(),
                    });
                }
                Ok(Err(e)) => {
                    warn!(
                        target: "exex::proposer::source",
                        url = %client.url,
                        error = %e,
                        "supervisor rpc error",
                    );
                    errs.push(e.to_string());
                }
                Err(_) => {
                    warn!(
                        target: "exex::proposer::source",
                        url = %client.url,
                        "supervisor rpc timed out",
                    );
                    errs.push("timed out".into());
                }
            }
        }
        Err(ProposalSourceError::Other(format!(
            "no available proposal sources: {}",
            errs.join("; ")
        )))
    }

    async fn sync_status(&self) -> Result<SyncStatus, ProposalSourceError> {
        let timeout = self.network_timeout;
        let futs = self.clients.iter().map(|c| async move {
            tokio::time::timeout(
                timeout,
                c.provider.raw_request::<_, RawSupervisorSyncStatus>(
                    Cow::Borrowed("supervisor_syncStatus"),
                    (),
                ),
            )
            .await
        });
        let results = join_all(futs).await;

        let mut earliest: Option<RawSupervisorSyncStatus> = None;
        let mut errs: Vec<String> = Vec::new();
        for (i, res) in results.into_iter().enumerate() {
            match res {
                Ok(Ok(s)) => {
                    if earliest
                        .as_ref()
                        .map(|e| s.min_synced_l1.number < e.min_synced_l1.number)
                        .unwrap_or(true)
                    {
                        earliest = Some(s);
                    }
                }
                Ok(Err(e)) => {
                    warn!(
                        target: "exex::proposer::source",
                        idx = i,
                        error = %e,
                        "supervisor sync status error",
                    );
                    errs.push(e.to_string());
                }
                Err(_) => {
                    warn!(target: "exex::proposer::source", idx = i, "supervisor sync status timed out");
                    errs.push("timed out".into());
                }
            }
        }

        match earliest {
            Some(e) => Ok(SyncStatus {
                current_l1: e.min_synced_l1.into(),
                safe_l2: e.safe_timestamp,
                finalized_l2: e.finalized_timestamp,
            }),
            None => Err(ProposalSourceError::Other(format!(
                "no available sync status sources: {}",
                errs.join("; ")
            ))),
        }
    }

    async fn close(&self) {}
}
