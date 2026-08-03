use alloy_primitives::{B256, BlockNumber};
use async_trait::async_trait;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;
use world_chain_proof_metrics::{
    RPC_TARGET_L2_CONSENSUS, record_l2_finalized_block, record_rpc_request,
};

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
    /// One endpoint in a verifying pair failed.
    #[error("{endpoint} consensus endpoint failed: {source}")]
    Endpoint {
        endpoint: &'static str,
        #[source]
        source: Box<Self>,
    },
    /// Primary and verifying endpoints returned different output roots for the same L2 block.
    #[error(
        "consensus output root mismatch at L2 block {l2_block_number}: primary {primary}, verifying {verifying}"
    )]
    OutputRootMismatch {
        l2_block_number: u64,
        primary: B256,
        verifying: B256,
    },
}

impl ConsensusError {
    fn endpoint(endpoint: &'static str, source: Self) -> Self {
        Self::Endpoint {
            endpoint,
            source: Box::new(source),
        }
    }
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
        let block_number = sync_status.finalized_l2.number;
        record_l2_finalized_block(block_number);
        Ok(block_number)
    }
}

/// Consensus provider that requires two independent providers to return identical results.
///
/// When no verifying provider is configured, requests are delegated directly to the primary.
/// When one is configured, output-root requests run concurrently and any error or disagreement
/// fails the operation without selecting either result. The primary alone determines the latest
/// finalized height because independently synchronized clients may advance that moving head at
/// slightly different times.
#[derive(Debug, Clone)]
pub struct VerifyingConsensusProvider<C> {
    primary: C,
    verifying: Option<C>,
}

impl<C> VerifyingConsensusProvider<C> {
    /// Creates a provider with an optional strict-agreement verifier.
    pub const fn new(primary: C, verifying: Option<C>) -> Self {
        Self { primary, verifying }
    }
}

#[async_trait]
impl<C> ConsensusProvider for VerifyingConsensusProvider<C>
where
    C: ConsensusProvider,
{
    async fn output_root_at_block(&self, l2_block_number: u64) -> Result<B256, ConsensusError> {
        let Some(verifying) = &self.verifying else {
            return self.primary.output_root_at_block(l2_block_number).await;
        };

        let (primary_result, verifying_result) = futures_util::join!(
            self.primary.output_root_at_block(l2_block_number),
            verifying.output_root_at_block(l2_block_number),
        );
        let primary = primary_result.map_err(|error| ConsensusError::endpoint("primary", error))?;
        let verifying =
            verifying_result.map_err(|error| ConsensusError::endpoint("verifying", error))?;

        if primary != verifying {
            return Err(ConsensusError::OutputRootMismatch {
                l2_block_number,
                primary,
                verifying,
            });
        }
        Ok(primary)
    }

    async fn latest_l2_finalized_block(&self) -> Result<BlockNumber, ConsensusError> {
        self.primary.latest_l2_finalized_block().await
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct MockConsensusProvider {
        output_root: Result<B256, &'static str>,
        finalized_block: Result<BlockNumber, &'static str>,
    }

    #[async_trait]
    impl ConsensusProvider for MockConsensusProvider {
        async fn output_root_at_block(
            &self,
            _l2_block_number: u64,
        ) -> Result<B256, ConsensusError> {
            self.output_root
                .map_err(|error| ConsensusError::Rpc(error.into()))
        }

        async fn latest_l2_finalized_block(&self) -> Result<BlockNumber, ConsensusError> {
            self.finalized_block
                .map_err(|error| ConsensusError::Rpc(error.into()))
        }
    }

    fn provider(
        output_root: Result<B256, &'static str>,
        finalized_block: Result<BlockNumber, &'static str>,
    ) -> MockConsensusProvider {
        MockConsensusProvider {
            output_root,
            finalized_block,
        }
    }

    #[tokio::test]
    async fn delegates_to_primary_without_verifier() {
        let root = B256::repeat_byte(0x11);
        let consensus = VerifyingConsensusProvider::new(provider(Ok(root), Ok(42)), None);

        assert_eq!(consensus.output_root_at_block(10).await.unwrap(), root);
        assert_eq!(consensus.latest_l2_finalized_block().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn accepts_matching_verifying_results() {
        let root = B256::repeat_byte(0x22);
        let consensus = VerifyingConsensusProvider::new(
            provider(Ok(root), Ok(42)),
            Some(provider(Ok(root), Ok(41))),
        );

        assert_eq!(consensus.output_root_at_block(10).await.unwrap(), root);
        assert_eq!(consensus.latest_l2_finalized_block().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn rejects_output_root_mismatch() {
        let primary = B256::repeat_byte(0x11);
        let verifying = B256::repeat_byte(0x22);
        let consensus = VerifyingConsensusProvider::new(
            provider(Ok(primary), Ok(42)),
            Some(provider(Ok(verifying), Ok(42))),
        );

        assert!(matches!(
            consensus.output_root_at_block(10).await,
            Err(ConsensusError::OutputRootMismatch {
                l2_block_number: 10,
                primary: actual_primary,
                verifying: actual_verifying,
            }) if actual_primary == primary && actual_verifying == verifying
        ));
    }

    #[tokio::test]
    async fn fails_when_verifying_endpoint_fails() {
        let root = B256::repeat_byte(0x11);
        let consensus = VerifyingConsensusProvider::new(
            provider(Ok(root), Ok(42)),
            Some(provider(Err("unavailable"), Ok(42))),
        );

        assert!(matches!(
            consensus.output_root_at_block(10).await,
            Err(ConsensusError::Endpoint {
                endpoint: "verifying",
                ..
            })
        ));
    }
}
