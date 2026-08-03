use alloy_primitives::{B256, BlockNumber};
use async_trait::async_trait;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;
use world_chain_proof_metrics::{RPC_TARGET_L2_CONSENSUS, record_rpc_request};

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

    async fn request<T>(
        &self,
        method: &'static str,
        params: Value,
        missing_result: ConsensusError,
    ) -> Result<T, ConsensusError>
    where
        T: DeserializeOwned,
    {
        let result = self.request_inner(method, params, missing_result).await;
        record_rpc_request(RPC_TARGET_L2_CONSENSUS, method, result.is_ok());
        result
    }

    async fn request_inner<T>(
        &self,
        method: &'static str,
        params: Value,
        missing_result: ConsensusError,
    ) -> Result<T, ConsensusError>
    where
        T: DeserializeOwned,
    {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
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
            .json::<JsonRpcResponse<T>>()
            .await
            .map_err(|error| ConsensusError::Rpc(error.to_string()))?;

        if let Some(error) = response.error {
            return Err(ConsensusError::Rpc(format!(
                "json-rpc error {}: {}",
                error.code, error.message
            )));
        }
        response.result.ok_or(missing_result)
    }
}

#[async_trait]
impl ConsensusProvider for OptimismConsensusClient {
    async fn output_root_at_block(&self, l2_block_number: u64) -> Result<B256, ConsensusError> {
        let output: OutputAtBlockResponse = self
            .request(
                "optimism_outputAtBlock",
                serde_json::json!([format!("0x{l2_block_number:x}")]),
                ConsensusError::MissingOutputRoot,
            )
            .await?;
        output
            .output_root
            .parse()
            .map_err(|error| ConsensusError::Rpc(format!("invalid outputRoot: {error}")))
    }

    async fn latest_l2_finalized_block(&self) -> Result<BlockNumber, ConsensusError> {
        let sync_status: SyncStatusResponse = self
            .request(
                "optimism_syncStatus",
                serde_json::json!([]),
                ConsensusError::FinalizedBlockNotFound,
            )
            .await?;
        Ok(sync_status.finalized_l2.number)
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
