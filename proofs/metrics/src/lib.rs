//! Shared metrics for proof services.

use std::task::{Context, Poll};

use alloy_json_rpc::{RequestPacket, ResponsePacket};
use alloy_primitives::{Address, utils::format_ether};
use alloy_provider::Provider;
use alloy_rpc_client::{ClientBuilder, RpcClient};
use alloy_transport::{BoxFuture, TransportError};
use telemetry_batteries::reexports::metrics;
use tower::{Layer, Service};
use tracing::warn;
use url::Url;

/// Ethereum L1 execution RPC target label.
pub const RPC_TARGET_L1_EXECUTION: &str = "l1_execution";
/// OP consensus-client RPC target label.
pub const RPC_TARGET_L2_CONSENSUS: &str = "l2_consensus";

/// Current transaction-sending wallet balance in ETH.
pub const METRICS_WALLET_BALANCE_ETH: &str = "wallet.balance_eth";
/// Completed outbound RPC requests.
pub const METRICS_RPC_CLIENT_REQUESTS: &str = "rpc.client.requests";

/// Registers shared metric descriptions.
pub fn describe_metrics() {
    metrics::describe_gauge!(
        METRICS_WALLET_BALANCE_ETH,
        metrics::Unit::Count,
        "Current L1 transaction-sending wallet balance in ETH."
    );
    metrics::describe_counter!(
        METRICS_RPC_CLIENT_REQUESTS,
        metrics::Unit::Count,
        "Completed outbound RPC requests by target, method, and outcome."
    );
}

/// Refreshes a transaction-sending wallet's balance gauge.
pub async fn refresh_wallet_balance<P>(provider: &P, address: Address)
where
    P: Provider,
{
    let gauge = metrics::gauge!(METRICS_WALLET_BALANCE_ETH, "address" => address.to_string());
    match provider.get_balance(address).await {
        Ok(balance) => match format_ether(balance).parse::<f64>() {
            Ok(balance_eth) => gauge.set(balance_eth),
            Err(error) => warn!(%address, %error, "failed to convert wallet balance to ETH"),
        },
        Err(error) => warn!(%address, %error, "failed to fetch wallet balance"),
    }
}

/// Builds an Alloy HTTP client that counts completed RPC outcomes.
pub fn metered_http_client(url: Url, target: &'static str) -> RpcClient {
    ClientBuilder::default()
        .layer(RpcMetricsLayer { target })
        .http(url)
}

#[derive(Debug, Clone, Copy)]
struct RpcMetricsLayer {
    target: &'static str,
}

impl<S> Layer<S> for RpcMetricsLayer {
    type Service = RpcMetricsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RpcMetricsService {
            target: self.target,
            inner,
        }
    }
}

#[derive(Debug, Clone)]
struct RpcMetricsService<S> {
    target: &'static str,
    inner: S,
}

impl<S> Service<RequestPacket> for RpcMetricsService<S>
where
    S: Service<RequestPacket, Response = ResponsePacket, Error = TransportError>
        + Clone
        + Send
        + Sync
        + 'static,
    S::Future: Send,
{
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: RequestPacket) -> Self::Future {
        let target = self.target;
        let method = request
            .as_single()
            .map_or_else(|| "batch".to_owned(), |request| request.method().to_owned());
        let mut inner = self.inner.clone();

        Box::pin(async move {
            let result = inner.call(request).await;
            let success = matches!(&result, Ok(response) if response.is_success());
            record_rpc_request(target, method, success);
            result
        })
    }
}

/// Records the outcome of a completed chain RPC request.
pub fn record_rpc_request(
    target: &'static str,
    method: impl Into<metrics::SharedString>,
    success: bool,
) {
    metrics::counter!(
        METRICS_RPC_CLIENT_REQUESTS,
        "target" => target,
        "method" => method.into(),
        "outcome" => if success { "success" } else { "error" },
    )
    .increment(1);
}
