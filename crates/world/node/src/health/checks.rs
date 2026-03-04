use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tokio::time::Instant;

use alloy_consensus::BlockHeader;
use async_trait::async_trait;
use reth_network_api::{NetworkInfo, PeersInfo};
use reth_provider::{BlockNumReader, HeaderProvider};

use super::{CheckResult, HealthCheck};

pub struct HeartbeatCheck;

#[async_trait]
impl HealthCheck for HeartbeatCheck {
    fn name(&self) -> &'static str {
        "heartbeat"
    }

    async fn check(&self) -> CheckResult {
        CheckResult {
            healthy: true,
            detail: None,
        }
    }
}

pub struct MinPeersCheck<N> {
    pub min: usize,
    pub network: N,
}

#[async_trait]
impl<N: PeersInfo + Send + Sync + 'static> HealthCheck for MinPeersCheck<N> {
    fn name(&self) -> &'static str {
        "min_peers"
    }

    async fn check(&self) -> CheckResult {
        let count = self.network.num_connected_peers();
        let healthy = count >= self.min;
        CheckResult {
            healthy,
            detail: (!healthy).then(|| format!("peers: {count}/{}", self.min)),
        }
    }
}

pub struct NotSyncingCheck<N> {
    pub network: N,
}

#[async_trait]
impl<N: NetworkInfo + Send + Sync + 'static> HealthCheck for NotSyncingCheck<N> {
    fn name(&self) -> &'static str {
        "not_syncing"
    }

    async fn check(&self) -> CheckResult {
        let syncing = self.network.is_syncing();
        CheckResult {
            healthy: !syncing,
            detail: syncing.then(|| "node is syncing".to_string()),
        }
    }
}

/// Which block number to track for progress.
#[derive(Debug, Clone, Copy, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BlockNumberMode {
    /// Canonical head — blocks fully executed. Use for liveness/readiness.
    #[default]
    Best,
    /// Last stored — includes downloaded-but-not-yet-executed blocks. Use for startup during IBD.
    Last,
}

/// Fails if the tracked block number has not advanced within `period`.
/// On the first call the baseline is established and the check returns healthy.
pub struct BlockProgressCheck<P> {
    pub period: Duration,
    pub mode: BlockNumberMode,
    pub provider: P,
    /// (last_block, time_we_first_saw_it_stuck)
    state: Mutex<Option<(u64, Instant)>>,
}

impl<P> BlockProgressCheck<P> {
    pub fn new(period: Duration, mode: BlockNumberMode, provider: P) -> Self {
        Self {
            period,
            mode,
            provider,
            state: Mutex::new(None),
        }
    }
}

#[async_trait]
impl<P: BlockNumReader + Send + Sync + 'static> HealthCheck for BlockProgressCheck<P> {
    fn name(&self) -> &'static str {
        "block_progress"
    }

    async fn check(&self) -> CheckResult {
        let current = match match self.mode {
            BlockNumberMode::Best => self.provider.best_block_number(),
            BlockNumberMode::Last => self.provider.last_block_number(),
        } {
            Ok(n) => n,
            Err(e) => {
                return CheckResult {
                    healthy: false,
                    detail: Some(format!("provider error: {e}")),
                };
            }
        };

        let mut state = self
            .state
            .lock()
            .expect("block progress check mutex poisoned");
        match *state {
            None => {
                *state = Some((current, Instant::now()));
                CheckResult {
                    healthy: true,
                    detail: None,
                }
            }
            Some((last_block, stuck_since)) => {
                if current > last_block {
                    *state = Some((current, Instant::now()));
                    CheckResult {
                        healthy: true,
                        detail: None,
                    }
                } else {
                    let elapsed = stuck_since.elapsed();
                    let healthy = elapsed < self.period;
                    CheckResult {
                        healthy,
                        detail: (!healthy).then(|| {
                            format!(
                                "block stuck at {current} for {elapsed:.0?}, limit {:.0?}",
                                self.period
                            )
                        }),
                    }
                }
            }
        }
    }
}

/// Fails if the latest block's timestamp is older than `max_age` relative to wall-clock time.
pub struct BlockTimestampCheck<P> {
    pub max_age: Duration,
    pub provider: P,
}

#[async_trait]
impl<P> HealthCheck for BlockTimestampCheck<P>
where
    P: BlockNumReader + HeaderProvider + Send + Sync + 'static,
{
    fn name(&self) -> &'static str {
        "block_timestamp"
    }

    async fn check(&self) -> CheckResult {
        let block_num = match self.provider.best_block_number() {
            Ok(n) => n,
            Err(e) => {
                return CheckResult {
                    healthy: false,
                    detail: Some(format!("provider error: {e}")),
                };
            }
        };

        let header = match self.provider.header_by_number(block_num) {
            Ok(Some(h)) => h,
            Ok(None) => {
                return CheckResult {
                    healthy: false,
                    detail: Some(format!("no header for block {block_num}")),
                };
            }
            Err(e) => {
                return CheckResult {
                    healthy: false,
                    detail: Some(format!("provider error: {e}")),
                };
            }
        };

        let block_ts = header.timestamp();
        let now_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let age_secs = now_ts.saturating_sub(block_ts);
        let max_age_secs = self.max_age.as_secs();
        let healthy = age_secs <= max_age_secs;
        CheckResult {
            healthy,
            detail: (!healthy)
                .then(|| format!("latest block is {age_secs}s old, limit {max_age_secs}s")),
        }
    }
}

/// Fails if available disk space on the filesystem containing `path` drops below `min_bytes`.
pub struct DiskSpaceCheck {
    pub path: PathBuf,
    pub min_bytes: u64,
}

fn available_bytes(path: &Path) -> eyre::Result<u64> {
    let stat = nix::sys::statvfs::statvfs(path)?;
    Ok(u64::from(stat.blocks_available()) * stat.fragment_size())
}

fn format_gib(bytes: u64) -> f64 {
    bytes as f64 / (1u64 << 30) as f64
}

#[async_trait]
impl HealthCheck for DiskSpaceCheck {
    fn name(&self) -> &'static str {
        "disk_space"
    }

    async fn check(&self) -> CheckResult {
        match available_bytes(&self.path) {
            Ok(avail) => {
                let healthy = avail >= self.min_bytes;
                CheckResult {
                    healthy,
                    detail: (!healthy).then(|| {
                        format!(
                            "{:.1} GiB free on {}, need {:.1} GiB",
                            format_gib(avail),
                            self.path.display(),
                            format_gib(self.min_bytes),
                        )
                    }),
                }
            }
            Err(e) => CheckResult {
                healthy: false,
                detail: Some(format!(
                    "failed to read disk stats for {}: {e}",
                    self.path.display()
                )),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    use alloy_primitives::{B256, BlockNumber};
    use reth_chainspec::ChainInfo;
    use reth_provider::{BlockHashReader, BlockNumReader, ProviderResult};

    use super::*;

    #[derive(Clone)]
    struct MockProvider(Arc<AtomicU64>);

    impl MockProvider {
        fn new(block: u64) -> Self {
            Self(Arc::new(AtomicU64::new(block)))
        }

        fn set(&self, block: u64) {
            self.0.store(block, Ordering::SeqCst);
        }
    }

    impl BlockHashReader for MockProvider {
        fn block_hash(&self, _number: BlockNumber) -> ProviderResult<Option<B256>> {
            Ok(None)
        }

        fn canonical_hashes_range(
            &self,
            _start: BlockNumber,
            _end: BlockNumber,
        ) -> ProviderResult<Vec<B256>> {
            Ok(vec![])
        }
    }

    impl BlockNumReader for MockProvider {
        fn chain_info(&self) -> ProviderResult<ChainInfo> {
            let n = self.0.load(Ordering::SeqCst);
            Ok(ChainInfo {
                best_hash: B256::ZERO,
                best_number: n,
            })
        }

        fn best_block_number(&self) -> ProviderResult<BlockNumber> {
            Ok(self.0.load(Ordering::SeqCst))
        }

        fn last_block_number(&self) -> ProviderResult<BlockNumber> {
            Ok(self.0.load(Ordering::SeqCst))
        }

        fn block_number(&self, _hash: B256) -> ProviderResult<Option<BlockNumber>> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn heartbeat_always_healthy() {
        let result = HeartbeatCheck.check().await;
        assert!(result.healthy);
        assert!(result.detail.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn block_progress_first_call_healthy() {
        let check = BlockProgressCheck::new(
            Duration::from_secs(60),
            BlockNumberMode::Best,
            MockProvider::new(10),
        );

        let result = check.check().await;
        assert!(result.healthy);
        assert!(result.detail.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn block_progress_advancing_healthy() {
        let provider = MockProvider::new(10);
        let check = BlockProgressCheck::new(
            Duration::from_secs(60),
            BlockNumberMode::Best,
            provider.clone(),
        );

        check.check().await;
        provider.set(11);
        let result = check.check().await;
        assert!(result.healthy);
        assert!(result.detail.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn block_progress_stuck_within_period_healthy() {
        let check = BlockProgressCheck::new(
            Duration::from_secs(60),
            BlockNumberMode::Best,
            MockProvider::new(10),
        );

        check.check().await;
        tokio::time::advance(Duration::from_secs(30)).await;
        let result = check.check().await;
        assert!(result.healthy);
    }

    #[tokio::test(start_paused = true)]
    async fn block_progress_stuck_past_period_unhealthy() {
        let check = BlockProgressCheck::new(
            Duration::from_secs(60),
            BlockNumberMode::Best,
            MockProvider::new(10),
        );

        check.check().await;
        tokio::time::advance(Duration::from_secs(61)).await;
        let result = check.check().await;
        assert!(!result.healthy);
        assert!(result.detail.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn block_progress_reset_after_advance() {
        let provider = MockProvider::new(10);
        let check = BlockProgressCheck::new(
            Duration::from_secs(60),
            BlockNumberMode::Best,
            provider.clone(),
        );

        check.check().await;
        tokio::time::advance(Duration::from_secs(61)).await;
        provider.set(11);
        let result = check.check().await;
        assert!(result.healthy);
        assert!(result.detail.is_none());
    }

    // ---- BlockTimestampCheck tests ----

    use alloy_consensus::Header as AlloyHeader;
    use reth_primitives_traits::SealedHeader;
    use std::ops::RangeBounds;

    #[derive(Clone)]
    struct MockTimestampProvider {
        block_num: u64,
        block_timestamp: u64,
    }

    impl BlockHashReader for MockTimestampProvider {
        fn block_hash(&self, _num: BlockNumber) -> ProviderResult<Option<B256>> {
            Ok(None)
        }
        fn canonical_hashes_range(
            &self,
            _s: BlockNumber,
            _e: BlockNumber,
        ) -> ProviderResult<Vec<B256>> {
            Ok(vec![])
        }
    }

    impl BlockNumReader for MockTimestampProvider {
        fn chain_info(&self) -> ProviderResult<ChainInfo> {
            Ok(ChainInfo {
                best_hash: B256::ZERO,
                best_number: self.block_num,
            })
        }
        fn best_block_number(&self) -> ProviderResult<BlockNumber> {
            Ok(self.block_num)
        }
        fn last_block_number(&self) -> ProviderResult<BlockNumber> {
            Ok(self.block_num)
        }
        fn block_number(&self, _hash: B256) -> ProviderResult<Option<BlockNumber>> {
            Ok(None)
        }
    }

    impl HeaderProvider for MockTimestampProvider {
        type Header = AlloyHeader;

        fn header(
            &self,
            _hash: alloy_primitives::BlockHash,
        ) -> ProviderResult<Option<Self::Header>> {
            Ok(None)
        }

        fn header_by_number(&self, _num: BlockNumber) -> ProviderResult<Option<Self::Header>> {
            Ok(Some(AlloyHeader {
                timestamp: self.block_timestamp,
                ..Default::default()
            }))
        }

        fn headers_range(
            &self,
            _range: impl RangeBounds<BlockNumber>,
        ) -> ProviderResult<Vec<Self::Header>> {
            Ok(vec![])
        }

        fn sealed_header(
            &self,
            _num: BlockNumber,
        ) -> ProviderResult<Option<SealedHeader<Self::Header>>> {
            Ok(None)
        }

        fn sealed_headers_while(
            &self,
            _range: impl RangeBounds<BlockNumber>,
            _predicate: impl FnMut(&SealedHeader<Self::Header>) -> bool,
        ) -> ProviderResult<Vec<SealedHeader<Self::Header>>> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn block_timestamp_recent_healthy() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let provider = MockTimestampProvider {
            block_num: 1,
            block_timestamp: now - 10,
        };
        let check = BlockTimestampCheck {
            max_age: Duration::from_secs(60),
            provider,
        };
        let result = check.check().await;
        assert!(result.healthy);
        assert!(result.detail.is_none());
    }

    #[tokio::test]
    async fn block_timestamp_stale_unhealthy() {
        let provider = MockTimestampProvider {
            block_num: 1,
            block_timestamp: 0,
        };
        let check = BlockTimestampCheck {
            max_age: Duration::from_secs(60),
            provider,
        };
        let result = check.check().await;
        assert!(!result.healthy);
        assert!(result.detail.is_some());
    }

    // ---- DiskSpaceCheck tests ----

    #[tokio::test]
    async fn disk_space_zero_min_bytes_healthy() {
        let check = DiskSpaceCheck {
            path: std::env::temp_dir(),
            min_bytes: 0,
        };
        let result = check.check().await;
        assert!(result.healthy);
    }

    #[tokio::test]
    async fn disk_space_u64_max_unhealthy() {
        let check = DiskSpaceCheck {
            path: std::env::temp_dir(),
            min_bytes: u64::MAX,
        };
        let result = check.check().await;
        assert!(!result.healthy);
        assert!(result.detail.is_some());
    }

    #[tokio::test]
    async fn disk_space_bad_path_unhealthy() {
        let check = DiskSpaceCheck {
            path: PathBuf::from("/nonexistent/path/that/does/not/exist"),
            min_bytes: 0,
        };
        let result = check.check().await;
        assert!(!result.healthy);
        assert!(result.detail.is_some());
    }
}
