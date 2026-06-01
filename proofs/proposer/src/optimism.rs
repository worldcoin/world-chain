use alloy_primitives::B256;
use async_trait::async_trait;
use serde::Deserialize;

use crate::{OutputRootProvider, ProposerError};

/// HTTP client for `optimism_outputAtBlock`.
#[derive(Debug, Clone)]
pub struct OptimismOutputRootClient {
    client: reqwest::Client,
    rpc_url: String,
}

impl OptimismOutputRootClient {
    /// Creates a new output-root client from the provided consensus client rpc endpoint.
    pub fn new(rpc_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            rpc_url: rpc_url.into(),
        }
    }
}

#[async_trait]
impl OutputRootProvider for OptimismOutputRootClient {
    async fn output_root_at_block(&self, l2_block_number: u64) -> Result<B256, ProposerError> {
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
            .map_err(|error| ProposerError::Rpc(error.to_string()))?
            .error_for_status()
            .map_err(|error| ProposerError::Rpc(error.to_string()))?
            .json::<JsonRpcResponse<OutputAtBlockResponse>>()
            .await
            .map_err(|error| ProposerError::Rpc(error.to_string()))?;

        if let Some(error) = response.error {
            return Err(ProposerError::Rpc(format!(
                "json-rpc error {}: {}",
                error.code, error.message
            )));
        }

        let output = response.result.ok_or(ProposerError::MissingOutputRoot)?;
        output
            .output_root
            .parse()
            .map_err(|error| ProposerError::Rpc(format!("invalid outputRoot: {error}")))
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
