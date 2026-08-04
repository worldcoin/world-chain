//! Shared metrics for proof services.

use std::{
    task::{Context, Poll},
    time::Duration,
};

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
/// Latest finalized L2 block reported by the OP consensus client.
pub const METRICS_L2_FINALIZED_BLOCK_NUMBER: &str = "l2.finalized_block_number";
/// Completed outbound RPC requests.
pub const METRICS_RPC_CLIENT_REQUESTS: &str = "rpc.client.requests";
/// Confirmed challenge transactions.
pub const METRICS_CHALLENGES_SUBMITTED: &str = "challenges.submitted";
/// Confirmed on-chain proof-lane submissions.
pub const METRICS_PROOF_LANES_SUBMITTED: &str = "proof_lanes.submitted";
/// Newly created durable proof requests.
pub const METRICS_PROOF_REQUESTS_CREATED: &str = "proof_requests.created";
/// Proof jobs claimed by workers.
pub const METRICS_PROOF_JOBS_CLAIMED: &str = "proof_jobs.claimed";
/// Completed worker proof-job attempts.
pub const METRICS_PROOF_JOBS_COMPLETED: &str = "proof_jobs.completed";
/// End-to-end worker proof-job attempt duration.
pub const METRICS_PROOF_JOB_DURATION_SECONDS: &str = "proof_job.duration_seconds";
/// Whether this worker's enclave signing key is registered on-chain.
pub const METRICS_ENCLAVE_KEY_REGISTERED: &str = "enclave_key.registered";
/// Enclave key registration attempts, by outcome.
pub const METRICS_ENCLAVE_REGISTRATION_ATTEMPTS: &str = "enclave_key.registration_attempts";

/// Registers shared metric descriptions.
pub fn describe_metrics() {
    metrics::describe_gauge!(
        METRICS_WALLET_BALANCE_ETH,
        metrics::Unit::Count,
        "Current L1 transaction-sending wallet balance in ETH."
    );
    metrics::describe_gauge!(
        METRICS_L2_FINALIZED_BLOCK_NUMBER,
        metrics::Unit::Count,
        "Latest finalized L2 block reported by the OP consensus client."
    );
    metrics::describe_counter!(
        METRICS_RPC_CLIENT_REQUESTS,
        metrics::Unit::Count,
        "Completed outbound RPC requests by target, method, and outcome."
    );
    metrics::describe_counter!(
        METRICS_CHALLENGES_SUBMITTED,
        metrics::Unit::Count,
        "Number of challenge transactions successfully confirmed on L1."
    );
    metrics::describe_counter!(
        METRICS_PROOF_LANES_SUBMITTED,
        metrics::Unit::Count,
        "Number of proof-lane transactions successfully confirmed on L1."
    );
    metrics::describe_counter!(
        METRICS_PROOF_REQUESTS_CREATED,
        metrics::Unit::Count,
        "Number of newly created durable proof requests by backend."
    );
    metrics::describe_counter!(
        METRICS_PROOF_JOBS_CLAIMED,
        metrics::Unit::Count,
        "Number of proof jobs claimed by workers by backend."
    );
    metrics::describe_counter!(
        METRICS_PROOF_JOBS_COMPLETED,
        metrics::Unit::Count,
        "Number of completed worker proof-job attempts by backend and outcome."
    );
    metrics::describe_histogram!(
        METRICS_PROOF_JOB_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "End-to-end worker proof-job attempt duration by backend and outcome."
    );
    metrics::describe_gauge!(
        METRICS_ENCLAVE_KEY_REGISTERED,
        metrics::Unit::Count,
        "1 when this worker's enclave signing key is registered on-chain, 0 otherwise. \
         A worker holding 0 leases no proof jobs, because proofs signed by an unregistered \
         key do not verify."
    );
    metrics::describe_counter!(
        METRICS_ENCLAVE_REGISTRATION_ATTEMPTS,
        metrics::Unit::Count,
        "Enclave key registration attempts by outcome (registered, already_registered, failed)."
    );
}

/// Sets the enclave-key registration gauge.
///
/// Emitted eagerly at `0` on startup so the "never registered" case is a visible zero rather
/// than an absent series that a threshold monitor would silently ignore.
pub fn set_enclave_key_registered(registered: bool) {
    metrics::gauge!(METRICS_ENCLAVE_KEY_REGISTERED).set(if registered { 1.0 } else { 0.0 });
}

/// Records an enclave key registration attempt and its outcome.
pub fn increment_enclave_registration_attempts(outcome: &'static str) {
    metrics::counter!(METRICS_ENCLAVE_REGISTRATION_ATTEMPTS, "outcome" => outcome).increment(1);
}

/// Updates the latest finalized L2 block gauge.
pub fn record_l2_finalized_block(block_number: u64) {
    metrics::gauge!(METRICS_L2_FINALIZED_BLOCK_NUMBER).set(block_number as f64);
}

/// Records a successfully confirmed challenge transaction.
pub fn increment_challenges_submitted() {
    metrics::counter!(METRICS_CHALLENGES_SUBMITTED).increment(1);
}

/// Records a successfully confirmed proof-lane transaction.
pub fn increment_proof_lanes_submitted(lane: &'static str) {
    metrics::counter!(METRICS_PROOF_LANES_SUBMITTED, "lane" => lane).increment(1);
}

/// Records a newly created durable proof request.
pub fn increment_proof_requests_created(backend: &'static str) {
    metrics::counter!(METRICS_PROOF_REQUESTS_CREATED, "backend" => backend).increment(1);
}

/// Records a proof job claimed by a worker.
pub fn increment_proof_jobs_claimed(backend: &'static str) {
    metrics::counter!(METRICS_PROOF_JOBS_CLAIMED, "backend" => backend).increment(1);
}

/// Records a completed worker proof-job attempt and its duration.
pub fn record_proof_job_completed(
    backend: &'static str,
    outcome: &'static str,
    duration: Duration,
) {
    metrics::counter!(
        METRICS_PROOF_JOBS_COMPLETED,
        "backend" => backend,
        "outcome" => outcome,
    )
    .increment(1);
    metrics::histogram!(
        METRICS_PROOF_JOB_DURATION_SECONDS,
        "backend" => backend,
        "outcome" => outcome,
    )
    .record(duration.as_secs_f64());
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

/// Renders an RPC endpoint for logs with any embedded credential removed.
///
/// Provider API keys are routinely carried in the URL path (Alchemy, Infura) or in userinfo,
/// so logging an endpoint verbatim ships the credential to the log backend. Keep the scheme,
/// host and port — enough to tell endpoints apart when triaging — and drop everything that
/// could be a secret. Unparseable input degrades to the scheme only rather than echoing it
/// back, since a URL we cannot parse is a URL whose secret we cannot locate.
pub fn redact_endpoint(url: &str) -> String {
    match Url::parse(url) {
        Ok(parsed) => match parsed.host_str() {
            Some(host) => match parsed.port() {
                Some(port) => format!("{}://{host}:{port}", parsed.scheme()),
                None => format!("{}://{host}", parsed.scheme()),
            },
            None => format!("{}://<no-host>", parsed.scheme()),
        },
        Err(_) => "<unparseable-url>".to_string(),
    }
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

#[cfg(test)]
mod tests {
    use super::redact_endpoint;

    #[test]
    fn redacts_path_embedded_api_keys() {
        assert_eq!(
            redact_endpoint("https://eth-sepolia.g.alchemy.com/v2/Ux-X_RqdTZesXUQidgBqZ"),
            "https://eth-sepolia.g.alchemy.com"
        );
    }

    #[test]
    fn redacts_userinfo_and_query_credentials() {
        assert_eq!(
            redact_endpoint("https://user:pass@rpc.example.com/path?apikey=secret"),
            "https://rpc.example.com"
        );
    }

    #[test]
    fn keeps_port_so_in_cluster_endpoints_stay_distinguishable() {
        assert_eq!(
            redact_endpoint("http://op-node-0.alphanet-world-chain-node.svc.cluster.local:9545"),
            "http://op-node-0.alphanet-world-chain-node.svc.cluster.local:9545"
        );
    }

    #[test]
    fn unparseable_input_does_not_echo_back() {
        let redacted = redact_endpoint("not a url with a secret=hunter2 in it");
        assert_eq!(redacted, "<unparseable-url>");
        assert!(!redacted.contains("hunter2"));
    }
}
