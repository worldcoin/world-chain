//! The environment an acceptance test runs against.
//!
//! An [`Env`] pairs a committed [`NetworkManifest`] with live RPC endpoints. The
//! manifest describes the target network configuration. The acceptance catalog
//! itself is derived from test code.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_eips::BlockNumberOrTag;
use alloy_primitives::TxHash;
use alloy_provider::{Provider, RootProvider};
use alloy_rpc_client::RpcClient;
use alloy_rpc_types_eth::TransactionReceipt;
use alloy_transport::layers::RetryBackoffLayer;
use alloy_transport_http::reqwest::{
    Client,
    header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue},
};
use eyre::eyre::{WrapErr, bail, eyre};
use reth_rpc_layer::{JwtSecret, secret_to_bearer_header};
use serde::Serialize;
use url::Url;
use world_chain_chainspec::{Feature, NetworkManifest, WorldChainHardfork, hardfork_key};

/// A JWT-authenticated Engine API endpoint.
///
/// The Engine API (`authrpc`) requires a bearer token derived from a shared JWT
/// secret with a short validity window, so a fresh token is minted for every
/// [`EngineApi::provider`] call rather than cached.
#[derive(Clone)]
pub struct EngineApi {
    url: Url,
    secret: JwtSecret,
}

impl EngineApi {
    /// The Engine API URL.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// A freshly JWT-authenticated provider over the Engine API.
    ///
    /// A new bearer token is generated per call so it never outlives the JWT
    /// validity window on long runs. Engine methods are reached through the raw
    /// client, e.g. `engine.provider()?.client().request("engine_…", params)`.
    pub fn provider(&self) -> eyre::Result<RootProvider> {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, secret_to_bearer_header(&self.secret));
        let http = Client::builder()
            .default_headers(headers)
            .build()
            .wrap_err("failed to build Engine API HTTP client")?;
        let client = RpcClient::builder().http_with_client(http, self.url.clone());
        Ok(RootProvider::new(client))
    }
}

/// Default timeouts and budgets applied to tests. Overridable per run.
#[derive(Clone, Copy, Debug)]
pub struct Thresholds {
    /// Maximum time to wait for the block number to advance.
    pub block_advance_timeout: Duration,
    /// Poll interval while waiting for block-number progress.
    pub block_poll_interval: Duration,
    /// Minimum required block-number increase for liveness tests.
    pub min_block_increments: u64,
    /// Upper bound on the observed average block time before it is a failure.
    pub max_block_time: Duration,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            block_advance_timeout: Duration::from_secs(60),
            block_poll_interval: Duration::from_secs(2),
            min_block_increments: 1,
            max_block_time: Duration::from_secs(4),
        }
    }
}

/// Cloudflare Access service-token credentials for reaching protected RPCs.
#[derive(Clone)]
pub struct CloudflareAccess {
    pub client_id: String,
    pub client_secret: String,
}

/// The target an acceptance run executes against.
#[derive(Clone)]
pub struct Env {
    network: String,
    manifest: Arc<NetworkManifest>,
    l2: RootProvider,
    l1: Option<RootProvider>,
    engine: Option<EngineApi>,
    flashblocks_url: Option<Url>,
    prometheus_url: Option<Url>,
    http: Client,
    thresholds: Thresholds,
}

impl Env {
    /// Start building an environment for the given committed manifest.
    pub fn builder(manifest: Arc<NetworkManifest>) -> EnvBuilder {
        EnvBuilder::new(manifest)
    }

    /// Network label used in logs and the report.
    pub fn network(&self) -> &str {
        &self.network
    }

    /// The committed manifest this run is held to.
    pub fn manifest(&self) -> &NetworkManifest {
        &self.manifest
    }

    /// L2 JSON-RPC provider.
    pub fn l2(&self) -> &RootProvider {
        &self.l2
    }

    /// L1 JSON-RPC provider, when an L1 endpoint was configured.
    pub fn l1(&self) -> Option<&RootProvider> {
        self.l1.as_ref()
    }

    /// JWT-authenticated Engine API, when an engine endpoint was configured.
    pub fn engine(&self) -> Option<&EngineApi> {
        self.engine.as_ref()
    }

    /// Flashblocks endpoint, when configured.
    pub fn flashblocks_url(&self) -> Option<&Url> {
        self.flashblocks_url.as_ref()
    }

    /// Prometheus endpoint, when configured.
    pub fn prometheus_url(&self) -> Option<&Url> {
        self.prometheus_url.as_ref()
    }

    /// Expected chain id, when the manifest pins one.
    pub fn expected_chain_id(&self) -> Option<u64> {
        self.manifest.chain.chain_id
    }

    /// Effective thresholds for this run.
    pub fn thresholds(&self) -> &Thresholds {
        &self.thresholds
    }

    /// The committed World Chain hardfork.
    pub fn hardfork(&self) -> WorldChainHardfork {
        self.manifest.hardfork
    }

    /// The committed feature list.
    pub fn features(&self) -> &[Feature] {
        &self.manifest.features
    }

    /// Whether the manifest commits to `feature`.
    pub fn commits_to_feature(&self, feature: Feature) -> bool {
        self.manifest.features.contains(&feature)
    }

    /// Whether the manifest commits to `fork` or any later hardfork.
    pub fn commits_to_hardfork(&self, fork: WorldChainHardfork) -> bool {
        fork.idx() <= self.manifest.hardfork.idx()
    }

    /// `eth_chainId` on the L2 provider.
    pub async fn chain_id(&self) -> eyre::Result<u64> {
        Ok(self.l2.get_chain_id().await?)
    }

    /// `eth_blockNumber` on the L2 provider.
    pub async fn block_number(&self) -> eyre::Result<u64> {
        Ok(self.l2.get_block_number().await?)
    }

    /// Latest L2 block.
    pub async fn latest_block(&self) -> eyre::Result<alloy_rpc_types_eth::Block> {
        self.l2
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await?
            .ok_or_else(|| eyre!("latest block missing"))
    }

    /// Block number tagged by `tag` (e.g. `Safe`, `Finalized`).
    pub async fn block_number_by_tag(&self, tag: BlockNumberOrTag) -> eyre::Result<Option<u64>> {
        Ok(self
            .l2
            .get_block_by_number(tag)
            .await?
            .map(|block| block.header.number))
    }

    /// `op_supportedCapabilities` on the L2 provider.
    pub async fn supported_capabilities(&self) -> eyre::Result<Vec<String>> {
        Ok(self
            .l2
            .client()
            .request_noparams::<Vec<String>>("op_supportedCapabilities")
            .await?)
    }

    /// Wait until the L2 chain reaches at least `target` block, returning the
    /// observed block number. Bounded by [`Thresholds::block_advance_timeout`]
    /// and polled at [`Thresholds::block_poll_interval`].
    pub async fn wait_for_block(&self, target: u64) -> eyre::Result<u64> {
        let deadline = Instant::now() + self.thresholds.block_advance_timeout;
        loop {
            let current = self.block_number().await?;
            if current >= target {
                return Ok(current);
            }
            if Instant::now() >= deadline {
                bail!(
                    "timed out after {:?} waiting for block {target}; last seen {current}",
                    self.thresholds.block_advance_timeout
                );
            }
            tokio::time::sleep(self.thresholds.block_poll_interval).await;
        }
    }

    /// Wait for the chain to advance by at least `delta` blocks from now,
    /// returning the final block number.
    pub async fn wait_for_blocks(&self, delta: u64) -> eyre::Result<u64> {
        let start = self.block_number().await?;
        self.wait_for_block(start + delta).await
    }

    /// Poll for a transaction receipt until it appears, bounded by
    /// [`Thresholds::block_advance_timeout`].
    pub async fn await_receipt(&self, tx: TxHash) -> eyre::Result<TransactionReceipt> {
        let deadline = Instant::now() + self.thresholds.block_advance_timeout;
        loop {
            if let Some(receipt) = self.l2.get_transaction_receipt(tx).await? {
                return Ok(receipt);
            }
            if Instant::now() >= deadline {
                bail!(
                    "timed out after {:?} waiting for receipt of {tx}",
                    self.thresholds.block_advance_timeout
                );
            }
            tokio::time::sleep(self.thresholds.block_poll_interval).await;
        }
    }

    /// Perform a GET against a URL using the shared (Cloudflare-aware) client,
    /// returning the response body as text.
    pub async fn http_get_text(&self, url: Url) -> eyre::Result<String> {
        let response = self
            .http
            .get(url.clone())
            .send()
            .await
            .wrap_err_with(|| format!("GET {url} failed"))?
            .error_for_status()
            .wrap_err_with(|| format!("GET {url} returned an error status"))?;
        response
            .text()
            .await
            .wrap_err_with(|| format!("reading body of GET {url} failed"))
    }

    /// A short description of the committed chain source.
    fn chain_source(&self) -> String {
        match (&self.manifest.chain.spec, &self.manifest.chain.genesis) {
            (Some(spec), _) => format!("spec:{spec}"),
            (_, Some(genesis)) => format!("genesis:{}", genesis.display()),
            _ => "unknown".to_string(),
        }
    }

    /// Structured summary embedded in the report.
    pub fn summary(&self) -> EnvSummary {
        EnvSummary {
            network: self.network.clone(),
            chain_source: self.chain_source(),
            expected_chain_id: self.manifest.chain.chain_id,
            hardfork: hardfork_key(self.manifest.hardfork).to_string(),
            features: self
                .manifest
                .features
                .iter()
                .map(ToString::to_string)
                .collect(),
            flashblocks: self.flashblocks_url.is_some(),
            metrics: self.prometheus_url.is_some(),
            l1: self.l1.is_some(),
            engine: self.engine.is_some(),
        }
    }
}

/// Serializable snapshot of an [`Env`] for the report header.
#[derive(Debug, Serialize)]
pub struct EnvSummary {
    pub network: String,
    pub chain_source: String,
    pub expected_chain_id: Option<u64>,
    pub hardfork: String,
    pub features: Vec<String>,
    pub flashblocks: bool,
    pub metrics: bool,
    pub l1: bool,
    pub engine: bool,
}

/// Builder for [`Env`].
pub struct EnvBuilder {
    network: Option<String>,
    manifest: Arc<NetworkManifest>,
    l2_rpc_url: Option<Url>,
    l1_rpc_url: Option<Url>,
    engine_url: Option<Url>,
    engine_jwt: Option<JwtSecret>,
    flashblocks_url: Option<Url>,
    prometheus_url: Option<Url>,
    cloudflare_access: Option<CloudflareAccess>,
    thresholds: Thresholds,
}

impl EnvBuilder {
    fn new(manifest: Arc<NetworkManifest>) -> Self {
        Self {
            network: None,
            manifest,
            l2_rpc_url: None,
            l1_rpc_url: None,
            engine_url: None,
            engine_jwt: None,
            flashblocks_url: None,
            prometheus_url: None,
            cloudflare_access: None,
            thresholds: Thresholds::default(),
        }
    }

    /// Network label override (defaults to the manifest name).
    pub fn network(mut self, network: impl Into<String>) -> Self {
        self.network = Some(network.into());
        self
    }

    /// Set the required L2 JSON-RPC endpoint.
    pub fn l2_rpc_url(mut self, url: Url) -> Self {
        self.l2_rpc_url = Some(url);
        self
    }

    /// Set the optional L1 JSON-RPC endpoint.
    pub fn l1_rpc_url(mut self, url: Option<Url>) -> Self {
        self.l1_rpc_url = url;
        self
    }

    /// Set the optional Engine API endpoint and its JWT secret. Both must be
    /// provided together or both omitted.
    pub fn engine(mut self, url: Option<Url>, jwt: Option<JwtSecret>) -> Self {
        self.engine_url = url;
        self.engine_jwt = jwt;
        self
    }

    /// Set the optional flashblocks endpoint.
    pub fn flashblocks_url(mut self, url: Option<Url>) -> Self {
        self.flashblocks_url = url;
        self
    }

    /// Set the optional Prometheus endpoint.
    pub fn prometheus_url(mut self, url: Option<Url>) -> Self {
        self.prometheus_url = url;
        self
    }

    /// Set optional Cloudflare Access service-token credentials.
    pub fn cloudflare_access(mut self, access: Option<CloudflareAccess>) -> Self {
        self.cloudflare_access = access;
        self
    }

    /// Override the default thresholds for this run.
    pub fn thresholds(mut self, thresholds: Thresholds) -> Self {
        self.thresholds = thresholds;
        self
    }

    /// Build the environment, constructing the RPC providers and HTTP client.
    pub fn build(self) -> eyre::Result<Env> {
        let Some(l2_rpc_url) = self.l2_rpc_url else {
            bail!("an L2 RPC URL is required to build an acceptance environment");
        };

        let network = self.network.unwrap_or_else(|| self.manifest.name.clone());

        let engine = match (self.engine_url, self.engine_jwt) {
            (Some(url), Some(secret)) => Some(EngineApi { url, secret }),
            (None, None) => None,
            _ => bail!("engine URL and JWT secret must be provided together"),
        };

        let http = http_client(self.cloudflare_access.as_ref())?;
        let l2 = provider(http.clone(), l2_rpc_url);
        let l1 = self.l1_rpc_url.map(|url| provider(http.clone(), url));

        Ok(Env {
            network,
            manifest: self.manifest,
            l2,
            engine,
            l1,
            flashblocks_url: self.flashblocks_url,
            prometheus_url: self.prometheus_url,
            http,
            thresholds: self.thresholds,
        })
    }
}

/// Build a retrying [`RootProvider`] over the shared HTTP client.
fn provider(http: Client, url: Url) -> RootProvider {
    let retry = RetryBackoffLayer::new(4, 100, 330);
    let client = RpcClient::builder()
        .layer(retry)
        .http_with_client(http, url);
    RootProvider::new(client)
}

/// Build the shared HTTP client, injecting Cloudflare Access headers if present.
fn http_client(access: Option<&CloudflareAccess>) -> eyre::Result<Client> {
    let mut builder = Client::builder();

    if let Some(access) = access {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("cf-access-client-id"),
            HeaderValue::from_str(&access.client_id)
                .wrap_err("failed to build CF-Access-Client-Id header")?,
        );
        headers.insert(
            HeaderName::from_static("cf-access-client-secret"),
            HeaderValue::from_str(&access.client_secret)
                .wrap_err("failed to build CF-Access-Client-Secret header")?,
        );
        builder = builder.default_headers(headers);
    }

    builder
        .build()
        .wrap_err("failed to build acceptance HTTP client")
}
