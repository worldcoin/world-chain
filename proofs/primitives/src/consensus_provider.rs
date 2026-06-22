use alloy_primitives::{B256, BlockNumber};
use async_trait::async_trait;
use serde::Deserialize;
use thiserror::Error;

/// Source for all consensus clients requests.
#[async_trait]
pub trait ConsensusProvider: Send + Sync {
    /// Returns the output root for an L2 block number.
    async fn output_root_at_block(&self, l2_block_number: u64) -> Result<B256, ConsensusError>;

    /// Returns the highest L2 finalized block number.
    async fn latest_l2_finalized_block(&self) -> Result<BlockNumber, ConsensusError>;
}

#[derive(Error, Debug)]
pub enum ConsensusError {
    /// The output-root RPC response did not contain an output root.
    #[error("optimism_outputAtBlock response did not contain an output root")]
    MissingOutputRoot,
    #[error("Latest L2 finalized block not found")]
    FinalizedBlockNotFound,
    /// RPC transport or JSON-RPC failure.
    #[error("rpc error: {0}")]
    Rpc(String),
}

/// HTTP client for OP consensus clients.
#[derive(Debug, Clone)]
pub struct OptimismConsensusClient {
    client: reqwest::Client,
    rpc_url: String,
}

impl OptimismConsensusClient {
    /// Creates a new output-root client from the provided consensus client rpc endpoint.
    pub fn new(rpc_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            rpc_url: rpc_url.into(),
        }
    }
}

#[async_trait]
impl ConsensusProvider for OptimismConsensusClient {
    async fn output_root_at_block(&self, l2_block_number: u64) -> Result<B256, ConsensusError> {
        let block = format!("0x{l2_block_number:x}");
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "optimism_outputAtBlock",
            "params": [block],
        });

        let response = self
            .client
            .post(&self.rpc_url)
            .json(&request)
            .send()
            .await
            .map_err(|error| ConsensusError::Rpc(error.to_string()))?
            .error_for_status()
            .map_err(|error| ConsensusError::Rpc(error.to_string()))?
            .json::<JsonRpcResponse<OutputAtBlockResponse>>()
            .await
            .map_err(|error| ConsensusError::Rpc(error.to_string()))?;

        if let Some(error) = response.error {
            return Err(ConsensusError::Rpc(format!(
                "json-rpc error {}: {}",
                error.code, error.message
            )));
        }

        let output = response.result.ok_or(ConsensusError::MissingOutputRoot)?;
        output
            .output_root
            .parse()
            .map_err(|error| ConsensusError::Rpc(format!("invalid outputRoot: {error}")))
    }

    async fn latest_l2_finalized_block(&self) -> Result<BlockNumber, ConsensusError> {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "optimism_syncStatus",
            "params": [],
        });

        let response = self
            .client
            .post(&self.rpc_url)
            .json(&request)
            .send()
            .await
            .map_err(|error| ConsensusError::Rpc(error.to_string()))?
            .error_for_status()
            .map_err(|error| ConsensusError::Rpc(error.to_string()))?
            .json::<JsonRpcResponse<SyncStatusResponse>>()
            .await
            .map_err(|error| ConsensusError::Rpc(error.to_string()))?;

        if let Some(error) = response.error {
            return Err(ConsensusError::Rpc(format!(
                "json-rpc error {}: {}",
                error.code, error.message
            )));
        }
        let sync_status_response = response
            .result
            .ok_or(ConsensusError::FinalizedBlockNotFound)?;

        Ok(sync_status_response.finalized_l2.number)
    }
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse<T> {
    result: Option<T>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
struct OutputAtBlockResponse {
    #[serde(rename = "outputRoot")]
    output_root: String,
}

#[derive(Debug, Deserialize)]
struct SyncStatusResponse {
    finalized_l2: L2BlockRef,
}

#[derive(Debug, Deserialize)]
struct L2BlockRef {
    number: BlockNumber,
}
