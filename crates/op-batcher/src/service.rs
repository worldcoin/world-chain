//! Batcher service: wires the L1 provider, store, metrics, driver, admin RPC,
//! and balance poller. Mirrors `op-batcher/batcher/service.go`.

use std::{net::SocketAddr, sync::Arc};

use jsonrpsee::server::ServerHandle;
use tokio_util::sync::CancellationToken;
use tracing::info;
use url::Url;

use crate::{
    Result,
    config::BatcherConfig,
    db::BatcherStore,
    driver::{AdminHandle, BatchSubmitter},
    error::OpBatcherError,
    metrics::{BatcherMetrics, spawn_balance_poller},
    provider::{L1Provider, L1ProviderConfig, ProviderError, SignerKind},
    rpc::{AdminRpcError, start_admin_server},
    source::LocalBlockSource,
};

/// Fully assembled batcher service.
pub struct BatcherService {
    pub driver: Arc<BatchSubmitter>,
    pub store: Arc<BatcherStore>,
    admin_rpc: Option<(SocketAddr, ServerHandle)>,
    balance_cancel: CancellationToken,
}

impl BatcherService {
    /// Build the batcher service with a pre-constructed (local) block source.
    pub async fn from_config_with_source(
        cfg: BatcherConfig,
        source: Arc<dyn LocalBlockSource>,
    ) -> Result<Self> {
        let metrics = Arc::new(BatcherMetrics::new());
        metrics.record_up();

        let signer = match (&cfg.private_key, &cfg.mnemonic) {
            (Some(pk), None) => SignerKind::PrivateKey(pk.clone()),
            (None, Some(phrase)) => SignerKind::Mnemonic {
                phrase: phrase.clone(),
                hd_path: cfg.hd_path.clone(),
            },
            (Some(_), Some(_)) => {
                return Err(OpBatcherError::msg(
                    "conflicting signer: provide only one of private-key / mnemonic",
                ));
            }
            (None, None) => {
                return Err(OpBatcherError::msg(
                    "missing signer: provide either private-key or mnemonic",
                ));
            }
        };

        let http_urls: std::result::Result<Vec<Url>, _> =
            cfg.l1_eth_rpcs.iter().map(|u| Url::parse(u)).collect();
        let http_urls = http_urls
            .map_err(|e| OpBatcherError::from(ProviderError::HttpClient(e.to_string())))?;

        let L1Provider { provider: l1, from } = L1ProviderConfig {
            http_urls,
            timeout: cfg.network_timeout,
            max_rate_limit_retries: cfg.rpc_max_retries,
            initial_backoff_ms: cfg.rpc_initial_backoff_ms,
            compute_units_per_second: cfg.rpc_compute_units_per_second,
            signer,
        }
        .build()?;
        info!(
            target: "exex::batcher::service",
            batcher = ?from,
            l1_rpcs = ?cfg.l1_eth_rpcs,
            batch_inbox = ?cfg.batch_inbox_address,
            "L1 provider initialized",
        );

        let store = Arc::new(BatcherStore::open(&cfg.datadir)?);
        info!(
            target: "exex::batcher::service",
            path = %store.path().display(),
            "batcher mdbx store opened",
        );

        let balance_cancel = CancellationToken::new();
        spawn_balance_poller(
            l1.clone(),
            from,
            cfg.balance_poll_interval,
            metrics.clone(),
            balance_cancel.clone(),
        );

        let driver = Arc::new(BatchSubmitter::new(
            cfg,
            source,
            l1,
            from,
            store.clone(),
            metrics.clone(),
        ));

        Ok(Self {
            driver,
            store,
            admin_rpc: None,
            balance_cancel,
        })
    }

    /// Start the driver (and the admin RPC server, if configured).
    pub async fn start(&mut self, cfg: &AdminRpcSettings) -> Result<()> {
        if cfg.enable {
            let addr: SocketAddr = format!("{}:{}", cfg.addr, cfg.port)
                .parse()
                .map_err(|e| OpBatcherError::from(AdminRpcError::Bind(format!("{e}"))))?;
            let handle: Arc<dyn crate::rpc::BatcherAdmin> =
                Arc::new(AdminHandle(self.driver.clone()));
            let (local, server) = start_admin_server(addr, handle).await?;
            self.admin_rpc = Some((local, server));
        }
        self.driver.start()?;
        Ok(())
    }

    /// Stop the driver, balance poller, and admin RPC server.
    pub async fn stop(&mut self) -> Result<()> {
        self.driver.stop_if_running().await;
        self.balance_cancel.cancel();
        if let Some((_, handle)) = self.admin_rpc.take() {
            let _ = handle.stop();
            handle.stopped().await;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct AdminRpcSettings {
    pub enable: bool,
    pub addr: String,
    pub port: u16,
}

impl AdminRpcSettings {
    pub fn from_config(cfg: &BatcherConfig) -> Self {
        Self {
            enable: cfg.rpc_enable_admin && cfg.rpc_port != 0,
            addr: cfg.rpc_addr.clone(),
            port: cfg.rpc_port,
        }
    }
}
