use std::{env, str::FromStr, time::Duration};

use eyre::eyre::{WrapErr, bail};
use url::Url;

const DEFAULT_BLOCK_ADVANCE_TIMEOUT_SECS: u64 = 60;
const DEFAULT_BLOCK_POLL_INTERVAL_SECS: u64 = 2;
const DEFAULT_MIN_BLOCK_INCREMENTS: u64 = 1;

#[derive(Clone)]
pub(super) struct Config {
    pub(super) network: String,
    pub(super) rpc_url: Url,
    pub(super) expected_chain_id: u64,
    pub(super) block_advance_timeout: Duration,
    pub(super) block_poll_interval: Duration,
    pub(super) min_block_increments: u64,
    pub(super) cloudflare_access: Option<CloudflareAccess>,
}

#[derive(Clone)]
pub(super) struct CloudflareAccess {
    pub(super) client_id: String,
    pub(super) client_secret: String,
}

impl Config {
    pub(super) fn from_env() -> eyre::Result<Option<Self>> {
        let Some(rpc_url) = optional_env("ACCEPTANCE_RPC_URL") else {
            eprintln!("ACCEPTANCE_RPC_URL not set, skipping acceptance tests");
            return Ok(None);
        };
        let Some(chain_id) = optional_env("ACCEPTANCE_CHAIN_ID") else {
            bail!("ACCEPTANCE_CHAIN_ID is required when ACCEPTANCE_RPC_URL is set");
        };
        let expected_chain_id = parse_value("ACCEPTANCE_CHAIN_ID", &chain_id)?;
        let cloudflare_access = cloudflare_access_from_env()?;

        Ok(Some(Self {
            network: optional_env("ACCEPTANCE_NETWORK").unwrap_or_else(|| "local".to_string()),
            rpc_url: rpc_url
                .parse()
                .wrap_err("failed to parse acceptance test RPC URL")?,
            expected_chain_id,
            block_advance_timeout: Duration::from_secs(parse_optional_value(
                "ACCEPTANCE_BLOCK_ADVANCE_TIMEOUT_SECS",
                DEFAULT_BLOCK_ADVANCE_TIMEOUT_SECS,
            )?),
            block_poll_interval: Duration::from_secs(parse_optional_value(
                "ACCEPTANCE_BLOCK_POLL_INTERVAL_SECS",
                DEFAULT_BLOCK_POLL_INTERVAL_SECS,
            )?),
            min_block_increments: parse_optional_value(
                "ACCEPTANCE_MIN_BLOCK_INCREMENTS",
                DEFAULT_MIN_BLOCK_INCREMENTS,
            )?,
            cloudflare_access,
        }))
    }

    pub(super) fn rpc_target(&self) -> String {
        let host = self.rpc_url.host_str().unwrap_or("<unknown-host>");
        let port = self
            .rpc_url
            .port()
            .map(|port| format!(":{port}"))
            .unwrap_or_default();

        format!("{}://{}{}/...", self.rpc_url.scheme(), host, port)
    }
}

fn cloudflare_access_from_env() -> eyre::Result<Option<CloudflareAccess>> {
    match (
        optional_env("CF_ACCESS_CLIENT_ID"),
        optional_env("CF_ACCESS_CLIENT_SECRET"),
    ) {
        (Some(client_id), Some(client_secret)) => Ok(Some(CloudflareAccess {
            client_id,
            client_secret,
        })),
        (None, None) => Ok(None),
        _ => bail!("CF_ACCESS_CLIENT_ID and CF_ACCESS_CLIENT_SECRET must be set together"),
    }
}

fn optional_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn parse_optional_value<T>(name: &str, default: T) -> eyre::Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    optional_env(name)
        .map(|value| parse_value(name, &value))
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn parse_value<T>(name: &str, value: &str) -> eyre::Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    value
        .parse()
        .wrap_err_with(|| format!("failed to parse {name}={value:?}"))
}
