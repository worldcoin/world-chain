//! Proposer service.
//!
//! Wires everything together: L1 provider, DGF instance, source, store,
//! driver, metrics, admin RPC, balance poller.
//!
//! Mirrors the single-chain slice of `op-proposer/proposer/service.go`.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use alloy_provider::DynProvider;
use jsonrpsee::server::ServerHandle;
use tokio_util::sync::CancellationToken;
use tracing::info;
use url::Url;

use crate::{
    config::ProposerConfig,
    contracts::DisputeGameFactory,
    db::ProposerStore,
    driver::L2OutputSubmitter,
    error::{OpProposerError, Result},
    metrics::{ProposerMetrics, spawn_balance_poller},
    provider::{L1Provider, L1ProviderConfig, ProviderError, SignerKind},
    rpc::{AdminRpcError, start_admin_server},
    source::{ProposalSource, rollup::RollupProposalSource},
};

/// Fully assembled proposer service.
pub struct ProposerService {
    pub driver: Arc<L2OutputSubmitter>,
    pub source: Arc<dyn ProposalSource>,
    pub factory: DisputeGameFactory,
    pub l1: DynProvider,
    pub store: Arc<ProposerStore>,
    pub metrics: Arc<ProposerMetrics>,
    admin_rpc: Option<(SocketAddr, ServerHandle)>,
    balance_cancel: CancellationToken,
}

impl ProposerService {
    /// Build the proposer service using a [`RollupProposalSource`].
    ///
    /// Requires `--proposer.rollup-rpc`. When running as an ExEx, prefer
    /// [`ProposerService::from_config_with_source`] with a
    /// [`LocalProposalSource`](crate::source::local::LocalProposalSource)
    /// to skip the rollup-RPC round-trip entirely.
    pub async fn from_config(cfg: ProposerConfig) -> Result<Self> {
        if cfg.rollup_rpcs.is_empty() {
            return Err(OpProposerError::msg(
                "missing proposal source: pass --proposer.rollup-rpc or use \
                 ProposerService::from_config_with_source",
            ));
        }
        let source: Arc<dyn ProposalSource> = Arc::new(RollupProposalSource::new(
            cfg.rollup_rpcs.clone(),
            cfg.network_timeout,
            cfg.active_sequencer_check_duration,
        )?);
        Self::from_config_with_source(cfg, source).await
    }

    /// Build the proposer service with a pre-constructed proposal source.
    ///
    /// This is the preferred constructor for the ExEx, which builds a
    /// [`LocalProposalSource`](crate::source::local::LocalProposalSource)
    /// over the in-process node provider.
    pub async fn from_config_with_source(
        cfg: ProposerConfig,
        source: Arc<dyn ProposalSource>,
    ) -> Result<Self> {
        let metrics = Arc::new(ProposerMetrics::new());
        metrics.record_up();

        // L1 provider — wallet-equipped, with fallback, retry, cached nonces,
        // 3/2 gas fallback filler, blob gas estimation, and chain-id pre-fetch.
        let signer = match (&cfg.private_key, &cfg.mnemonic) {
            (Some(pk), None) => SignerKind::PrivateKey(pk.clone()),
            (None, Some(phrase)) => SignerKind::Mnemonic {
                phrase: phrase.clone(),
                hd_path: cfg.hd_path.clone(),
            },
            (Some(_), Some(_)) => {
                return Err(OpProposerError::msg(
                    "conflicting signer: provide only one of private-key / mnemonic",
                ));
            }
            (None, None) => {
                return Err(OpProposerError::msg(
                    "missing signer: provide either private-key or mnemonic",
                ));
            }
        };

        let http_urls: std::result::Result<Vec<Url>, _> =
            cfg.l1_eth_rpcs.iter().map(|u| Url::parse(u)).collect();
        let http_urls = http_urls
            .map_err(|e| OpProposerError::from(ProviderError::HttpClient(e.to_string())))?;

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
            target: "exex::proposer::service",
            proposer = ?from,
            l1_rpcs = ?cfg.l1_eth_rpcs,
            "L1 provider initialized",
        );

        // DGF instance over the wallet-equipped provider — used both for
        // reads (gameCount, gameAtIndex, initBonds, version) and for the
        // write path (`create(..).send()` in the driver).
        let factory = DisputeGameFactory::new(cfg.game_factory_address, l1.clone());
        let version = factory.version().await?;
        info!(
            target: "exex::proposer::service",
            address = ?cfg.game_factory_address,
            version,
            "connected to DisputeGameFactory",
        );

        let store = Arc::new(ProposerStore::open(&cfg.datadir)?);
        info!(
            target: "exex::proposer::service",
            path = %store.path().display(),
            "proposer mdbx store opened",
        );

        // Wallet balance poller.
        let balance_cancel = CancellationToken::new();
        spawn_balance_poller(
            l1.clone(),
            from,
            cfg.balance_poll_interval,
            metrics.clone(),
            balance_cancel.clone(),
        );

        let driver = Arc::new(L2OutputSubmitter::new(
            cfg,
            source.clone(),
            factory.clone(),
            l1.clone(),
            from,
            metrics.clone(),
            store.clone(),
        ));

        Ok(Self {
            driver,
            source,
            factory,
            l1,
            store,
            metrics,
            admin_rpc: None,
            balance_cancel,
        })
    }

    /// Start the driver (and the admin RPC server, if configured).
    pub async fn start(&mut self, cfg: &AdminRpcSettings) -> Result<()> {
        if cfg.enable {
            let addr: SocketAddr = format!("{}:{}", cfg.addr, cfg.port)
                .parse()
                .map_err(|e| OpProposerError::from(AdminRpcError::Bind(format!("{e}"))))?;
            let (local, handle) = start_admin_server(addr, self.driver.clone()).await?;
            self.admin_rpc = Some((local, handle));
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
        self.source.close().await;
        Ok(())
    }

    /// Default balance-poll interval used by tests / examples.
    pub const DEFAULT_BALANCE_POLL_INTERVAL: Duration = Duration::from_secs(60);
}

#[derive(Debug, Clone)]
pub struct AdminRpcSettings {
    pub enable: bool,
    pub addr: String,
    pub port: u16,
}

impl AdminRpcSettings {
    pub fn from_config(cfg: &ProposerConfig) -> Self {
        Self {
            enable: cfg.rpc_enable_admin && cfg.rpc_port != 0,
            addr: cfg.rpc_addr.clone(),
            port: cfg.rpc_port,
        }
    }
}
