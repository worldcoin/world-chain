use alloy_json_rpc::{RequestPacket, ResponsePacket};
use alloy_provider::transport::{TransportError, TransportErrorKind, TransportFut};
use alloy_rpc_client::RpcClient;
use alloy_transport_http::Http;
use backoff::{ExponentialBackoff, backoff::Backoff};
use std::{
    task::{Context, Poll},
    time::Duration,
};
use tower::Service;

const MAX_ATTEMPTS: u32 = 3;

/// Apply retries below the provider so receipt polling and transaction fillers
/// get the same policy as explicit reads. Submission requests are never retried.
pub fn client(endpoint: &str, field: &'static str) -> eyre::Result<RpcClient> {
    let url = crate::args::rpc_url(endpoint, field)?;
    let client = reqwest::Client::builder()
        .timeout(super::DEFAULT_TIMEOUT)
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
    async fn retry_read(
        mut self,
        request: RequestPacket,
    ) -> Result<ResponsePacket, TransportError> {
        let methods = request.method_names().collect::<Vec<_>>().join(",");
        let mut backoff = ExponentialBackoff::default();
        let mut attempt = 0;
        loop {
            attempt += 1;
            let error = match self
                .inner
                .call(request.clone())
                .await
                .and_then(check_response)
            {
                Ok(response) => return Ok(response),
                Err(error) => error,
            };
            if attempt >= MAX_ATTEMPTS || !is_transient(&error) {
                return Err(error);
            }
            let Some(delay) = backoff.next_backoff() else {
                return Err(error);
            };
            log_retry(self.dependency, &methods, attempt, delay, &error);
            tokio::time::sleep(delay).await;
        }
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
