use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use reth_network_api::{NetworkInfo, PeersInfo};
use reth_provider::{BlockNumReader, HeaderProvider};
use serde::Deserialize;

use super::{
    HealthCheck, HealthServer, Probe,
    checks::{
        BlockNumberMode, BlockProgressCheck, BlockTimestampCheck, DiskSpaceCheck, HeartbeatCheck,
        MinPeersCheck, NotSyncingCheck,
    },
};

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct HealthConfig {
    pub startup: ProbeConfig,
    pub readiness: ProbeConfig,
    pub liveness: ProbeConfig,
}

#[derive(Debug, Deserialize)]
pub struct ProbeConfig {
    #[serde(default = "defaults::interval_secs")]
    pub interval_secs: u64,
    #[serde(default)]
    pub checks: Vec<CheckConfig>,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            interval_secs: defaults::interval_secs(),
            checks: vec![],
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CheckConfig {
    BlockProgress {
        #[serde(default = "defaults::period_secs")]
        period_secs: u64,
        #[serde(default)]
        mode: BlockNumberMode,
    },
    BlockTimestamp {
        #[serde(default = "defaults::max_age_secs")]
        max_age_secs: u64,
    },
    DiskSpace {
        path: PathBuf,
        #[serde(default = "defaults::min_gb")]
        min_gb: f64,
    },
    MinPeers {
        #[serde(default = "defaults::min_peers")]
        min: usize,
    },
    NotSyncing,
    Heartbeat,
}

mod defaults {
    pub fn interval_secs() -> u64 {
        30
    }
    pub fn period_secs() -> u64 {
        60
    }
    pub fn max_age_secs() -> u64 {
        60
    }
    pub fn min_gb() -> f64 {
        10.0
    }
    pub fn min_peers() -> usize {
        1
    }
}

impl HealthConfig {
    pub fn from_file(path: &Path) -> eyre::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    }

    pub fn build<P, N>(self, addr: SocketAddr, provider: P, network: N) -> HealthServer
    where
        P: BlockNumReader + HeaderProvider + Clone + Send + Sync + 'static,
        N: PeersInfo + NetworkInfo + Clone + Send + Sync + 'static,
    {
        HealthServer::new(
            build_probe(self.startup, &provider, &network),
            build_probe(self.readiness, &provider, &network),
            build_probe(self.liveness, &provider, &network),
            addr,
        )
    }
}

fn build_probe<P, N>(config: ProbeConfig, provider: &P, network: &N) -> Probe
where
    P: BlockNumReader + HeaderProvider + Clone + Send + Sync + 'static,
    N: PeersInfo + NetworkInfo + Clone + Send + Sync + 'static,
{
    let checks: Vec<Arc<dyn HealthCheck>> = config
        .checks
        .into_iter()
        .map(|c| -> Arc<dyn HealthCheck> {
            match c {
                CheckConfig::Heartbeat => Arc::new(HeartbeatCheck),
                CheckConfig::MinPeers { min } => Arc::new(MinPeersCheck {
                    min,
                    network: network.clone(),
                }),
                CheckConfig::NotSyncing => Arc::new(NotSyncingCheck {
                    network: network.clone(),
                }),
                CheckConfig::BlockProgress { period_secs, mode } => {
                    Arc::new(BlockProgressCheck::new(
                        Duration::from_secs(period_secs),
                        mode,
                        provider.clone(),
                    ))
                }
                CheckConfig::BlockTimestamp { max_age_secs } => Arc::new(BlockTimestampCheck {
                    max_age: Duration::from_secs(max_age_secs),
                    provider: provider.clone(),
                }),
                CheckConfig::DiskSpace { path, min_gb } => Arc::new(DiskSpaceCheck {
                    path,
                    min_bytes: (min_gb * (1u64 << 30) as f64) as u64,
                }),
            }
        })
        .collect();
    Probe::new(checks, Duration::from_secs(config.interval_secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_deserializes() {
        let config: HealthConfig = serde_json::from_str("{}").unwrap();
        assert!(config.startup.checks.is_empty());
        assert!(config.readiness.checks.is_empty());
        assert!(config.liveness.checks.is_empty());
    }

    #[test]
    fn probe_config_interval_default() {
        let config: ProbeConfig = serde_json::from_str(r#"{"checks":[]}"#).unwrap();
        assert_eq!(config.interval_secs, 30);
    }

    #[test]
    fn probe_config_interval_explicit() {
        let config: ProbeConfig =
            serde_json::from_str(r#"{"interval_secs":10,"checks":[]}"#).unwrap();
        assert_eq!(config.interval_secs, 10);
    }

    #[test]
    fn heartbeat_check_deserializes() {
        let config: ProbeConfig =
            serde_json::from_str(r#"{"checks":[{"type":"heartbeat"}]}"#).unwrap();
        assert_eq!(config.checks.len(), 1);
        assert!(matches!(config.checks[0], CheckConfig::Heartbeat));
    }

    #[test]
    fn min_peers_check_with_default() {
        let config: ProbeConfig =
            serde_json::from_str(r#"{"checks":[{"type":"min_peers"}]}"#).unwrap();
        assert!(matches!(config.checks[0], CheckConfig::MinPeers { min: 1 }));
    }

    #[test]
    fn min_peers_check_explicit_value() {
        let config: ProbeConfig =
            serde_json::from_str(r#"{"checks":[{"type":"min_peers","min":3}]}"#).unwrap();
        assert!(matches!(config.checks[0], CheckConfig::MinPeers { min: 3 }));
    }

    #[test]
    fn not_syncing_check_deserializes() {
        let config: ProbeConfig =
            serde_json::from_str(r#"{"checks":[{"type":"not_syncing"}]}"#).unwrap();
        assert!(matches!(config.checks[0], CheckConfig::NotSyncing));
    }

    #[test]
    fn block_progress_defaults() {
        let config: ProbeConfig =
            serde_json::from_str(r#"{"checks":[{"type":"block_progress"}]}"#).unwrap();
        assert!(matches!(
            config.checks[0],
            CheckConfig::BlockProgress {
                period_secs: 60,
                mode: BlockNumberMode::Best
            }
        ));
    }

    #[test]
    fn block_progress_explicit_last_mode() {
        let config: ProbeConfig = serde_json::from_str(
            r#"{"checks":[{"type":"block_progress","period_secs":120,"mode":"last"}]}"#,
        )
        .unwrap();
        assert!(matches!(
            config.checks[0],
            CheckConfig::BlockProgress {
                period_secs: 120,
                mode: BlockNumberMode::Last
            }
        ));
    }

    #[test]
    fn block_timestamp_defaults() {
        let config: ProbeConfig =
            serde_json::from_str(r#"{"checks":[{"type":"block_timestamp"}]}"#).unwrap();
        assert!(matches!(
            config.checks[0],
            CheckConfig::BlockTimestamp { max_age_secs: 60 }
        ));
    }

    #[test]
    fn block_timestamp_explicit() {
        let config: ProbeConfig =
            serde_json::from_str(r#"{"checks":[{"type":"block_timestamp","max_age_secs":30}]}"#)
                .unwrap();
        assert!(matches!(
            config.checks[0],
            CheckConfig::BlockTimestamp { max_age_secs: 30 }
        ));
    }

    #[test]
    fn disk_space_deserializes() {
        let config: ProbeConfig =
            serde_json::from_str(r#"{"checks":[{"type":"disk_space","path":"/var/lib/data"}]}"#)
                .unwrap();
        assert!(matches!(config.checks[0], CheckConfig::DiskSpace { .. }));
        if let CheckConfig::DiskSpace { ref path, min_gb } = config.checks[0] {
            assert_eq!(path, std::path::Path::new("/var/lib/data"));
            assert!((min_gb - 10.0).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn full_health_config_deserializes() {
        let json = r#"{
            "startup":  {"checks":[{"type":"block_progress","mode":"last"}]},
            "readiness":{"checks":[{"type":"not_syncing"},{"type":"min_peers","min":2}]},
            "liveness": {"checks":[{"type":"heartbeat"},{"type":"block_timestamp"}]}
        }"#;
        let config: HealthConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.startup.checks.len(), 1);
        assert_eq!(config.readiness.checks.len(), 2);
        assert_eq!(config.liveness.checks.len(), 2);
    }
}
