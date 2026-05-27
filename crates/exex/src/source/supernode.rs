//! Supernode RPC backed proposal source (interop).
//!
//! Mirrors `op-proposer/proposer/source/source_supernode.go`.

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

pub struct SuperNodeProposalSource {
    clients: Vec<SuperNodeClient>,
    network_timeout: Duration,
}

struct SuperNodeClient {
    url: Url,
    provider: RootProvider,
}

impl SuperNodeProposalSource {
    pub fn new(urls: Vec<String>, network_timeout: Duration) -> Result<Self, ProposalSourceError> {
        if urls.is_empty() {
            return Err(ProposalSourceError::NoSources);
        }
        let mut clients = Vec::with_capacity(urls.len());
        for raw in urls {
            let url = Url::parse(&raw).map_err(|e| ProposalSourceError::Other(e.to_string()))?;
            let provider = RootProvider::new_http(url.clone());
            clients.push(SuperNodeClient { url, provider });
        }
        Ok(Self {
            clients,
            network_timeout,
        })
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RawL1BlockRef {
    hash: alloy_primitives::BlockHash,
    number: u64,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RawSuperRootData {
    super_root: B256,
    version: u8,
    super_root_bytes: alloy_primitives::Bytes,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RawSupernodeResponse {
    current_l1: RawL1BlockRef,
    current_safe_timestamp: u64,
    current_finalized_timestamp: u64,
    #[serde(default)]
    #[allow(dead_code)]
    timestamp: u64,
    data: Option<RawSuperRootData>,
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
impl ProposalSource for SuperNodeProposalSource {
    async fn proposal_at_sequence_num(
        &self,
        timestamp: u64,
    ) -> Result<Proposal, ProposalSourceError> {
        let mut errs: Vec<String> = Vec::new();
        for client in &self.clients {
            let fut = client.provider.raw_request::<_, RawSupernodeResponse>(
                Cow::Borrowed("supernode_superRootAtTimestamp"),
                (format!("{timestamp:#x}"),),
            );
            match tokio::time::timeout(self.network_timeout, fut).await {
                Ok(Ok(r)) => {
                    let Some(data) = r.data else {
                        errs.push(format!(
                            "supernode response has no super root data (ts {timestamp}, url {})",
                            client.url
                        ));
                        continue;
                    };
                    if data.version != SUPER_ROOT_VERSION_V1 {
                        errs.push(format!(
                            "unsupported super root version {} from supernode {}",
                            data.version, client.url
                        ));
                        continue;
                    }
                    return Ok(Proposal {
                        root: data.super_root,
                        sequence_num: timestamp,
                        super_root_marshalled: Some(data.super_root_bytes.to_vec()),
                        current_l1: r.current_l1.into(),
                        legacy: LegacyProposalData::default(),
                    });
                }
                Ok(Err(e)) => {
                    warn!(
                        target: "exex::proposer::source",
                        url = %client.url,
                        error = %e,
                        "supernode rpc error",
                    );
                    errs.push(e.to_string());
                }
                Err(_) => {
                    warn!(
                        target: "exex::proposer::source",
                        url = %client.url,
                        "supernode rpc timed out",
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
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let timeout = self.network_timeout;
        let futs = self.clients.iter().map(|c| async move {
            tokio::time::timeout(
                timeout,
                c.provider.raw_request::<_, RawSupernodeResponse>(
                    Cow::Borrowed("supernode_superRootAtTimestamp"),
                    (format!("{now:#x}"),),
                ),
            )
            .await
        });
        let results = join_all(futs).await;

        let mut earliest: Option<RawSupernodeResponse> = None;
        let mut errs: Vec<String> = Vec::new();
        for (i, res) in results.into_iter().enumerate() {
            match res {
                Ok(Ok(r)) => {
                    if earliest
                        .as_ref()
                        .map(|e| r.current_l1.number < e.current_l1.number)
                        .unwrap_or(true)
                    {
                        earliest = Some(r);
                    }
                }
                Ok(Err(e)) => {
                    warn!(target: "exex::proposer::source", idx = i, error = %e, "supernode sync status error");
                    errs.push(e.to_string());
                }
                Err(_) => {
                    warn!(target: "exex::proposer::source", idx = i, "supernode sync status timed out");
                    errs.push("timed out".into());
                }
            }
        }
        match earliest {
            Some(e) => Ok(SyncStatus {
                current_l1: e.current_l1.into(),
                safe_l2: e.current_safe_timestamp,
                finalized_l2: e.current_finalized_timestamp,
            }),
            None => Err(ProposalSourceError::Other(format!(
                "no available sync status sources: {}",
                errs.join("; ")
            ))),
        }
    }

    async fn close(&self) {}
}
