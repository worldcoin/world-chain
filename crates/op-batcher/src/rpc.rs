//! Admin RPC for the OP Batcher.
//!
//! Mirrors `op-batcher/rpc/api.go`. Exposes:
//! * `admin_startBatcher`
//! * `admin_stopBatcher`
//! * `admin_flushBatcher` (force-close + submit the open channel)
//!
//! The driver is reached through the [`BatcherAdmin`] trait so this module
//! doesn't depend on the concrete driver type.

use std::{net::SocketAddr, sync::Arc};

use async_trait::async_trait;
use jsonrpsee::{
    proc_macros::rpc,
    server::{ServerBuilder, ServerHandle},
    types::ErrorObjectOwned,
};
use tracing::info;

/// Control surface the admin RPC drives. Implemented by the batcher driver.
#[async_trait]
pub trait BatcherAdmin: Send + Sync + 'static {
    fn start(&self) -> Result<(), String>;
    async fn stop(&self) -> Result<(), String>;
    async fn flush(&self) -> Result<(), String>;
}

#[rpc(server, namespace = "admin")]
pub trait BatcherAdminApi {
    #[method(name = "startBatcher")]
    async fn start_batcher(&self) -> Result<(), ErrorObjectOwned>;

    #[method(name = "stopBatcher")]
    async fn stop_batcher(&self) -> Result<(), ErrorObjectOwned>;

    #[method(name = "flushBatcher")]
    async fn flush_batcher(&self) -> Result<(), ErrorObjectOwned>;
}

pub struct BatcherAdminRpc {
    driver: Arc<dyn BatcherAdmin>,
}

impl BatcherAdminRpc {
    pub fn new(driver: Arc<dyn BatcherAdmin>) -> Self {
        Self { driver }
    }
}

fn err(msg: String) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(-32000, msg, None::<()>)
}

#[async_trait]
impl BatcherAdminApiServer for BatcherAdminRpc {
    async fn start_batcher(&self) -> Result<(), ErrorObjectOwned> {
        self.driver.start().map_err(err)
    }

    async fn stop_batcher(&self) -> Result<(), ErrorObjectOwned> {
        self.driver.stop().await.map_err(err)
    }

    async fn flush_batcher(&self) -> Result<(), ErrorObjectOwned> {
        self.driver.flush().await.map_err(err)
    }
}

/// Start a jsonrpsee server bound to `addr` exposing the batcher admin RPC.
pub async fn start_admin_server(
    addr: SocketAddr,
    driver: Arc<dyn BatcherAdmin>,
) -> Result<(SocketAddr, ServerHandle), AdminRpcError> {
    let server = ServerBuilder::default()
        .build(addr)
        .await
        .map_err(|e| AdminRpcError::Bind(e.to_string()))?;
    let local = server
        .local_addr()
        .map_err(|e| AdminRpcError::Bind(e.to_string()))?;
    let handle = server.start(BatcherAdminRpc::new(driver).into_rpc());
    info!(target: "exex::batcher::rpc", addr = %local, "batcher admin RPC listening");
    Ok((local, handle))
}

#[derive(Debug, thiserror::Error)]
pub enum AdminRpcError {
    #[error("failed to bind admin rpc server: {0}")]
    Bind(String),
}
