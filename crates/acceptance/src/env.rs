//! The environment an acceptance check runs against.
//!
//! An [`Env`] pairs a committed [`NetworkManifest`] with live RPC endpoints. The
//! manifest is the acceptance criterion: it decides which checks run (a check
//! runs only when the manifest commits to its requirements) and what the network
//! must deliver. The same `Env` describes a freshly spawned devnet or a remote
//! alphanet behind Cloudflare Access.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use alloy_eips::BlockNumberOrTag;
use alloy_provider::{Provider, RootProvider};
use alloy_rpc_client::RpcClient;
use alloy_transport::layers::RetryBackoffLayer;
use alloy_transport_http::reqwest::{
    Client,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use eyre::eyre::{WrapErr, bail, eyre};
use serde::Serialize;
use url::Url;
use world_chain_manifest::NetworkManifest;

/// Default timeouts and budgets applied to checks. Overridable per run.
#[derive(Clone, Copy, Debug)]
pub struct Thresholds {
    /// Maximum time to wait for the block number to advance.
    pub block_advance_timeout: Duration,
    /// Poll interval while waiting for block-number progress.
    pub block_poll_interval: Duration,
    /// Minimum required block-number increase for liveness checks.
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
    committed: BTreeSet<String>,
    known: BTreeSet<String>,
    l2: RootProvider,
    l1: Option<RootProvider>,
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

    /// Requirement keys the manifest commits to.
    pub fn committed_requirements(&self) -> &BTreeSet<String> {
        &self.committed
    }

    /// Every requirement key the manifest recognises (committable or not). Used
    /// to reject `requires(...)` keys that name no known fork or feature.
    pub fn known_requirements(&self) -> &BTreeSet<String> {
        &self.known
    }

    /// Whether the manifest commits to the given requirement key.
    pub fn commits_to(&self, key: &str) -> bool {
        self.committed.contains(key)
    }

    /// Whether the manifest commits to every requirement in `required`.
    pub fn satisfies(&self, required: &[&str]) -> bool {
        required.iter().all(|key| self.committed.contains(*key))
    }

    /// Requirement keys in `required` the manifest does not commit to.
    pub fn missing(&self, required: &[&str]) -> Vec<String> {
        required
            .iter()
            .filter(|key| !self.committed.contains(**key))
            .map(|key| key.to_string())
            .collect()
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
            flashblocks: self.flashblocks_url.is_some(),
            metrics: self.prometheus_url.is_some(),
            l1: self.l1.is_some(),
            committed_requirements: self.committed.iter().cloned().collect(),
        }
    }
}

/// Serializable snapshot of an [`Env`] for the report header.
#[derive(Debug, Serialize)]
pub struct EnvSummary {
    pub network: String,
    pub chain_source: String,
    pub expected_chain_id: Option<u64>,
    pub flashblocks: bool,
    pub metrics: bool,
    pub l1: bool,
    pub committed_requirements: Vec<String>,
}

/// Builder for [`Env`].
pub struct EnvBuilder {
    network: Option<String>,
    manifest: Arc<NetworkManifest>,
    l2_rpc_url: Option<Url>,
    l1_rpc_url: Option<Url>,
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
            flashblocks_url: None,
            prometheus_url: None,
            cloudflare_access: None,
            thresholds: Thresholds::default(),
        }
    }

    /// Override the network label (defaults to the manifest name).
    pub fn network(mut self, network: impl Into<String>) -> Self {
        self.network = Some(network.into());
        self
    }

    /// L2 JSON-RPC URL. Required.
    pub fn l2_rpc_url(mut self, url: Url) -> Self {
        self.l2_rpc_url = Some(url);
        self
    }

    /// L1 JSON-RPC URL.
    pub fn l1_rpc_url(mut self, url: Option<Url>) -> Self {
        self.l1_rpc_url = url;
        self
    }

    /// Flashblocks endpoint.
    pub fn flashblocks_url(mut self, url: Option<Url>) -> Self {
        self.flashblocks_url = url;
        self
    }

    /// Prometheus endpoint.
    pub fn prometheus_url(mut self, url: Option<Url>) -> Self {
        self.prometheus_url = url;
        self
    }

    /// Cloudflare Access service token applied to every HTTP request.
    pub fn cloudflare_access(mut self, access: Option<CloudflareAccess>) -> Self {
        self.cloudflare_access = access;
        self
    }

    /// Override the default thresholds.
    pub fn thresholds(mut self, thresholds: Thresholds) -> Self {
        self.thresholds = thresholds;
        self
    }

    /// Build the environment, constructing the RPC providers and HTTP client.
    pub fn build(self) -> eyre::Result<Env> {
        let Some(l2_rpc_url) = self.l2_rpc_url else {
            bail!("an L2 RPC URL is required to build an acceptance environment");
        };

        let committed = self
            .manifest
            .committed_requirements()
            .wrap_err("failed to resolve committed requirements from manifest")?;
        let known = self
            .manifest
            .known_requirement_keys()
            .wrap_err("failed to resolve known requirement keys from manifest")?;
        let network = self.network.unwrap_or_else(|| self.manifest.name.clone());

        let http = http_client(self.cloudflare_access.as_ref())?;
        let l2 = provider(http.clone(), l2_rpc_url);
        let l1 = self.l1_rpc_url.map(|url| provider(http.clone(), url));

        Ok(Env {
            network,
            manifest: self.manifest,
            committed,
            known,
            l2,
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
