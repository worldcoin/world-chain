use alloy_eips::BlockNumberOrTag;
use alloy_provider::{Provider, RootProvider};
use alloy_rpc_client::RpcClient;
use alloy_transport::layers::RetryBackoffLayer;
use alloy_transport_http::reqwest::{
    Client,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use eyre::eyre::{WrapErr, eyre};

use super::config::{CloudflareAccess, Config};

#[derive(Clone)]
pub(super) struct RpcEnv {
    provider: RootProvider,
    l1_provider: Option<RootProvider>,
    bundler_provider: Option<RootProvider>,
    config: Config,
}

impl RpcEnv {
    pub(super) async fn connect() -> eyre::Result<Option<Self>> {
        reth_tracing::init_test_tracing();

        let Some(config) = Config::from_env()? else {
            return Ok(None);
        };
        let provider = provider_for_url(&config.rpc_url, config.cloudflare_access.as_ref())?;
        let l1_provider = config
            .karst_deposit
            .as_ref()
            .map(|deposit| provider_for_url(&deposit.l1_rpc_url, None))
            .transpose()?;
        let bundler_provider = config
            .bundler
            .as_ref()
            .map(|bundler| provider_for_url(&bundler.rpc_url, bundler.cloudflare_access.as_ref()))
            .transpose()?;

        Ok(Some(Self {
            provider,
            l1_provider,
            bundler_provider,
            config,
        }))
    }

    pub(super) fn config(&self) -> &Config {
        &self.config
    }

    pub(super) fn chain_provider(&self) -> &RootProvider {
        &self.provider
    }

    pub(super) fn l1_provider(&self) -> Option<&RootProvider> {
        self.l1_provider.as_ref()
    }

    pub(super) fn bundler_provider(&self) -> Option<&RootProvider> {
        self.bundler_provider.as_ref()
    }

    pub(super) async fn chain_id(&self) -> eyre::Result<u64> {
        Ok(self.provider.get_chain_id().await?)
    }

    pub(super) async fn latest_block_number(&self) -> eyre::Result<u64> {
        Ok(self.provider.get_block_number().await?)
    }

    pub(super) async fn latest_block(&self) -> eyre::Result<alloy_rpc_types_eth::Block> {
        self.provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await?
            .ok_or_else(|| eyre!("latest block missing"))
    }
}

fn provider_for_url(
    url: &url::Url,
    cloudflare_access: Option<&CloudflareAccess>,
) -> eyre::Result<RootProvider> {
    let retry = RetryBackoffLayer::new(4, 100, 330);
    let http_client = http_client(cloudflare_access)?;
    let client = RpcClient::builder()
        .layer(retry)
        .http_with_client(http_client, url.clone());

    Ok(RootProvider::new(client))
}

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
        .wrap_err("failed to build acceptance test HTTP client")
}
