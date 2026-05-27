//! Proposer service.
//!
//! Wires everything together: source, DGF contract, txmgr, store, driver,
//! metrics, admin RPC. Mirrors the single-chain slice of
//! `op-proposer/proposer/service.go`.

use std::{net::SocketAddr, sync::Arc};

use alloy_provider::DynProvider;
use jsonrpsee::server::ServerHandle;
use thiserror::Error;
use tracing::info;

use crate::{
    config::ProposerConfig,
    contracts::DisputeGameFactory,
    db::{ProposerStore, ProposerStoreError},
    driver::{L2OutputSubmitter, ProposerError},
    metrics::ProposerMetrics,
    rpc::{AdminRpcError, start_admin_server},
    source::{ProposalSource, ProposalSourceError, rollup::RollupProposalSource},
    txmgr::{SignerKey, TxManager, TxManagerConfig, TxManagerError},
};

/// Fully assembled proposer service.
pub struct ProposerService {
    pub driver: Arc<L2OutputSubmitter>,
    pub source: Arc<dyn ProposalSource>,
    pub factory: Arc<DisputeGameFactory<DynProvider>>,
    pub txmgr: Arc<TxManager>,
    pub store: Arc<ProposerStore>,
    pub metrics: Arc<ProposerMetrics>,

    admin_rpc: Option<(SocketAddr, ServerHandle)>,
}

impl ProposerService {
    /// Build the proposer service using a [`RollupProposalSource`].
    ///
    /// Requires `--proposer.rollup-rpc`. Prefer
    /// [`ProposerService::from_config_with_source`] when running as an ExEx
    /// — pass a [`LocalProposalSource`](crate::source::local::LocalProposalSource)
    /// backed by the node's own state and skip the rollup-rpc round-trip.
    pub async fn from_config(cfg: ProposerConfig) -> Result<Self, ServiceError> {
        if cfg.rollup_rpcs.is_empty() {
            return Err(ServiceError::MissingSource);
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
    /// [`LocalProposalSource`](crate::source::local::LocalProposalSource) over
    /// the in-process node provider.
    pub async fn from_config_with_source(
        cfg: ProposerConfig,
        source: Arc<dyn ProposalSource>,
    ) -> Result<Self, ServiceError> {
        let metrics = Arc::new(ProposerMetrics::new());
        metrics.record_up();

        let signer = match (&cfg.private_key, &cfg.mnemonic) {
            (Some(pk), None) => SignerKey::from_private_key_hex(pk)?,
            (None, Some(mnemonic)) => SignerKey::from_mnemonic(mnemonic, &cfg.hd_path)?,
            (Some(_), Some(_)) => return Err(ServiceError::ConflictingSigner),
            (None, None) => return Err(ServiceError::MissingSigner),
        };

        let txmgr = TxManager::new(
            &cfg.l1_eth_rpc,
            signer,
            TxManagerConfig {
                network_timeout: cfg.network_timeout,
                resubmission_timeout: cfg.resubmission_timeout,
                num_confirmations: cfg.num_confirmations,
            },
        )
        .await?;
        let txmgr = Arc::new(txmgr);
        info!(
            target: "exex::proposer::service",
            proposer = ?txmgr.from_address(),
            l1_rpc = %cfg.l1_eth_rpc,
            "tx manager initialized",
        );

        let factory = Arc::new(DisputeGameFactory::new(
            cfg.game_factory_address,
            txmgr.provider(),
            cfg.network_timeout,
        ));
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

        let driver = Arc::new(L2OutputSubmitter::new(
            cfg,
            source.clone(),
            factory.clone(),
            txmgr.clone(),
            metrics.clone(),
            store.clone(),
        ));

        Ok(Self {
            driver,
            source,
            factory,
            txmgr,
            store,
            metrics,
            admin_rpc: None,
        })
    }

    /// Start the driver (and the admin RPC server, if configured).
    pub async fn start(&mut self, cfg: &AdminRpcSettings) -> Result<(), ServiceError> {
        if cfg.enable {
            let addr: SocketAddr = format!("{}:{}", cfg.addr, cfg.port)
                .parse()
                .map_err(|e| ServiceError::Rpc(AdminRpcError::Bind(format!("{e}"))))?;
            let (local, handle) = start_admin_server(addr, self.driver.clone()).await?;
            self.admin_rpc = Some((local, handle));
        }
        self.driver.start()?;
        Ok(())
    }

    /// Stop the driver and admin RPC server.
    pub async fn stop(&mut self) -> Result<(), ServiceError> {
        self.driver.stop_if_running().await;
        if let Some((_, handle)) = self.admin_rpc.take() {
            let _ = handle.stop();
            handle.stopped().await;
        }
        self.source.close().await;
        self.txmgr.close();
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
    pub fn from_config(cfg: &ProposerConfig) -> Self {
        Self {
            enable: cfg.rpc_enable_admin && cfg.rpc_port != 0,
            addr: cfg.rpc_addr.clone(),
            port: cfg.rpc_port,
        }
    }
}

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("missing signer")]
    MissingSigner,
    #[error("conflicting signer")]
    ConflictingSigner,
    #[error("missing proposal source")]
    MissingSource,
    #[error(transparent)]
    TxManager(#[from] TxManagerError),
    #[error(transparent)]
    Contract(#[from] crate::contracts::ContractError),
    #[error(transparent)]
    Source(#[from] ProposalSourceError),
    #[error(transparent)]
    Store(#[from] ProposerStoreError),
    #[error(transparent)]
    Driver(#[from] ProposerError),
    #[error(transparent)]
    Rpc(#[from] AdminRpcError),
}
