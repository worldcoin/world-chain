use crate::{
    args,
    retry::{self, DEFAULT_TIMEOUT, RPC_MAX_ATTEMPTS},
    types::StepStage,
};
use alloy_json_rpc::{RequestPacket, ResponsePacket};
use alloy_provider::transport::{TransportError, TransportErrorKind, TransportFut};
use alloy_rpc_client::RpcClient;
use alloy_transport_http::Http;
use eyre::eyre::WrapErr;
use std::{
    task::{Context, Poll},
    time::Duration,
};
use tower::Service;

/// Bound the entire receipt wait, including any in-flight RPC at the deadline.
pub async fn receipt_with_deadline<T, E>(
    transaction_hash: alloy_primitives::B256,
    stage: StepStage,
    future: impl std::future::Future<Output = Result<T, E>>,
) -> eyre::Result<T>
where
    E: std::error::Error + Send + Sync + 'static,
{
    tokio::time::timeout(DEFAULT_TIMEOUT, future)
        .await
        .wrap_err_with(|| {
            format!("{stage} receipt timed out after 30 seconds; transaction {transaction_hash}")
        })?
        .wrap_err_with(|| format!("{stage} receipt failed; transaction {transaction_hash}"))
}

/// Apply retries below the provider so receipt polling and transaction fillers
/// get the same policy as explicit reads. Submission requests are never retried.
pub fn client(endpoint: &str, field: &'static str) -> eyre::Result<RpcClient> {
    let url = args::rpc_url(endpoint, field)?;
    let client = reqwest::Client::builder()
        .timeout(DEFAULT_TIMEOUT)
        .retry(reqwest::retry::never())
        .build()?;
    let inner = Http::with_client(client, url);
    let is_local = inner.guess_local();
    Ok(RpcClient::new(
        ReadRetryTransport {
            inner,
            dependency: field,
        },
        is_local,
    ))
}

#[derive(Clone, Debug)]
struct ReadRetryTransport {
    inner: Http<reqwest::Client>,
    dependency: &'static str,
}

impl Service<RequestPacket> for ReadRetryTransport {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: RequestPacket) -> Self::Future {
        // Submissions and mixed batches bypass retry handling entirely.
        if !request.method_names().all(is_read) {
            return self.inner.call(request);
        }
        Box::pin(self.clone().retry_read(request))
    }
}

impl ReadRetryTransport {
    async fn retry_read(self, request: RequestPacket) -> Result<ResponsePacket, TransportError> {
        let methods = request.method_names().collect::<Vec<_>>().join(",");
        let dependency = self.dependency;
        retry::with_backoff(
            RPC_MAX_ATTEMPTS,
            || {
                let mut transport = self.clone();
                let request = request.clone();
                async move { transport.inner.call(request).await.and_then(check_response) }
            },
            is_transient,
            |attempt, delay, error| log_retry(dependency, &methods, attempt, delay, error),
        )
        .await
    }
}

/// Surface transient JSON-RPC errors to the retry loop; preserve other responses.
fn check_response(response: ResponsePacket) -> Result<ResponsePacket, TransportError> {
    if let Some(error) = response.as_error() {
        let error = TransportError::ErrorResp(error.clone());
        if is_transient(&error) {
            return Err(error);
        }
    }
    Ok(response)
}

fn log_retry(
    dependency: &str,
    methods: &str,
    attempt: u32,
    delay: Duration,
    error: &TransportError,
) {
    let (failure_class, upstream_status, rpc_error_code) = match error {
        TransportError::Transport(TransportErrorKind::HttpError(error)) => {
            ("http", Some(error.status), None)
        }
        TransportError::Transport(TransportErrorKind::Custom(error))
            if error
                .downcast_ref::<reqwest::Error>()
                .is_some_and(|error| error.is_timeout()) =>
        {
            ("timeout", None, None)
        }
        TransportError::ErrorResp(error) => ("rpc", None, Some(error.code)),
        _ => ("transport", None, None),
    };
    tracing::warn!(
        dependency,
        rpc_method = methods,
        attempt,
        upstream_status,
        failure_class,
        rpc_error_code,
        retry_in_secs = delay.as_secs_f64(),
        "RPC read failed; retrying"
    );
}

fn is_read(method: &str) -> bool {
    matches!(
        method,
        "eth_call"
            | "eth_estimateGas"
            | "eth_chainId"
            | "eth_blockNumber"
            | "eth_gasPrice"
            | "eth_maxPriorityFeePerGas"
            | "eth_feeHistory"
            | "eth_getBlockByNumber"
            | "eth_getBlockByHash"
            | "eth_getBlockReceipts"
            | "eth_getTransactionReceipt"
            | "eth_getTransactionByHash"
            | "eth_getTransactionCount"
            | "eth_getBalance"
            | "eth_getCode"
            | "eth_getStorageAt"
            | "eth_getProof"
            | "eth_getLogs"
            | "net_version"
    )
}

fn is_transient(error: &TransportError) -> bool {
    match error {
        TransportError::Transport(TransportErrorKind::HttpError(error)) => {
            error.status == 408 || error.status == 429 || (500..600).contains(&error.status)
        }
        TransportError::Transport(TransportErrorKind::Custom(error)) => error
            .downcast_ref::<reqwest::Error>()
            .is_some_and(|error| error.is_timeout() || error.is_connect() || error.is_body()),
        TransportError::ErrorResp(error) => error.is_retry_err() || error.code == -32603,
        TransportError::NullResp => true,
        _ => false,
    }
}
