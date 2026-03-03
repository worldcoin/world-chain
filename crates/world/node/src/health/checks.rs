use std::{sync::Mutex, time::Duration};

use tokio::time::Instant;

use async_trait::async_trait;
use reth_network_api::{NetworkInfo, PeersInfo};
use reth_provider::BlockNumReader;

use super::{CheckResult, HealthCheck};

pub struct HeartbeatCheck;

#[async_trait]
impl HealthCheck for HeartbeatCheck {
    fn name(&self) -> &'static str {
        "heartbeat"
    }

    async fn check(&self) -> CheckResult {
        CheckResult { healthy: true, detail: None }
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
        Self { period, mode, provider, state: Mutex::new(None) }
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
                }
            }
        };

        let mut state = self.state.lock().expect("block progress check mutex poisoned");
        match *state {
            None => {
                *state = Some((current, Instant::now()));
                CheckResult { healthy: true, detail: None }
            }
            Some((last_block, stuck_since)) => {
                if current > last_block {
                    *state = Some((current, Instant::now()));
                    CheckResult { healthy: true, detail: None }
                } else {
                    let elapsed = stuck_since.elapsed();
                    let healthy = elapsed < self.period;
                    CheckResult {
                        healthy,
                        detail: (!healthy).then(|| {
                            format!("block stuck at {current} for {elapsed:.0?}, limit {:.0?}", self.period)
                        }),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use alloy_primitives::{BlockNumber, B256};
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
            Ok(ChainInfo { best_hash: B256::ZERO, best_number: n })
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
}
