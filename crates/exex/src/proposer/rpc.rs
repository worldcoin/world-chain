//! Admin RPC for the OP Proposer.
//!
//! Mirrors: [`op-proposer/proposer/rpc/api.go`][src] @ tag
//! `op-proposer/v1.16.3-rc.1`. Exposes:
//!
//! * `admin_startProposer` — [api.go L39–L41][src]
//! * `admin_stopProposer`  — [api.go L43–L45][src]
//!
//! The Go `ProposerDriver` interface (api.go L13–L16) is satisfied here by
//! `Arc<L2OutputSubmitter>` directly (via its inherent `start` / `stop`
//! methods).
//!
//! [src]:
//!     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/proposer/rpc/api.go

use std::{net::SocketAddr, sync::Arc};

use jsonrpsee::{
    core::async_trait,
    proc_macros::rpc,
    server::{ServerBuilder, ServerHandle},
    types::ErrorObjectOwned,
};
use tracing::info;

use crate::{Result, proposer::driver::L2OutputSubmitter};

#[rpc(server, namespace = "admin")]
pub trait ProposerAdminApi {
    #[method(name = "startProposer")]
    async fn start_proposer(&self) -> Result<(), ErrorObjectOwned>;

    #[method(name = "stopProposer")]
    async fn stop_proposer(&self) -> Result<(), ErrorObjectOwned>;
}

pub struct ProposerAdminRpc {
    driver: Arc<L2OutputSubmitter>,
}

impl ProposerAdminRpc {
    pub fn new(driver: Arc<L2OutputSubmitter>) -> Self {
        Self { driver }
    }
}

#[async_trait]
impl ProposerAdminApiServer for ProposerAdminRpc {
    async fn start_proposer(&self) -> Result<(), ErrorObjectOwned> {
        self.driver
            .start()
            .map_err(|e| ErrorObjectOwned::owned(-32000, e.to_string(), None::<()>))
    }

    async fn stop_proposer(&self) -> Result<(), ErrorObjectOwned> {
        self.driver
            .stop()
            .await
            .map_err(|e| ErrorObjectOwned::owned(-32000, e.to_string(), None::<()>))
    }
}

/// Start a jsonrpsee server bound to `addr` exposing the proposer admin RPC.
pub async fn start_admin_server(
    addr: SocketAddr,
    driver: Arc<L2OutputSubmitter>,
) -> Result<(SocketAddr, ServerHandle)> {
    let server = ServerBuilder::default()
        .build(addr)
        .await
        .map_err(|e| AdminRpcError::Bind(e.to_string()))?;
    let local = server
        .local_addr()
        .map_err(|e| AdminRpcError::Bind(e.to_string()))?;
    let handle = server.start(ProposerAdminRpc::new(driver).into_rpc());
    info!(target: "exex::proposer::rpc", addr = %local, "proposer admin RPC listening");
    Ok((local, handle))
}

#[derive(Debug, thiserror::Error)]
pub enum AdminRpcError {
    #[error("failed to bind admin rpc server: {0}")]
    Bind(String),
}
