use std::{env, str::FromStr, time::Duration};

use alloy_primitives::{Address, U256, address};
use eyre::eyre::{WrapErr, bail};
use url::Url;

const DEFAULT_BLOCK_ADVANCE_TIMEOUT_SECS: u64 = 60;
const DEFAULT_BLOCK_POLL_INTERVAL_SECS: u64 = 2;
const DEFAULT_MIN_BLOCK_INCREMENTS: u64 = 1;
const DEFAULT_USER_OPERATION_TIMEOUT_SECS: u64 = 30;
const DEFAULT_USER_OPERATION_REJECT_TIMEOUT_SECS: u64 = 3;
const DEFAULT_USER_OPERATION_POLL_INTERVAL_MS: u64 = 250;
const DEFAULT_USER_OPERATION_WALLET_COUNT: usize = 20;
const DEFAULT_USER_OPERATION_DEPLOY_CONCURRENCY: usize = 4;
const DEFAULT_USER_OPERATION_NONCE_CONCURRENCY: usize = 2;
const DEFAULT_USER_OPERATION_OWNER_START_INDEX: u32 = 1000;
const DEFAULT_USER_OPERATION_SPONSORSHIP_VALIDITY_SECS: u64 = 60;
const DEFAULT_USER_OPERATION_SPONSORSHIP_MAX_COST_WEI: &str = "1000000000000000000";
const WORLD_CHAIN_DEVNET_CHAIN_ID: u64 = 69420;
const WORLD_CHAIN_DEVNET_SAFE_4337_MODULE: Address =
    address!("70673A08a5B1086585d39979Fb2d84FDC0bB6Aaf");
const WORLD_CHAIN_DEVNET_SAFE_4337_WALLET_DEPLOYER: Address =
    address!("d1f0B51940DbD6e73891D2a41Ef14483fDC5Cb6e");
const WORLD_CHAIN_DEVNET_ENTRY_POINT_V0_7: Address =
    address!("0000000071727De22E5E9d8BAf0edAc6f37da032");

#[derive(Clone)]
pub(super) struct Config {
    pub(super) network: String,
    pub(super) rpc_url: Url,
    pub(super) expected_chain_id: u64,
    pub(super) block_advance_timeout: Duration,
    pub(super) block_poll_interval: Duration,
    pub(super) min_block_increments: u64,
    pub(super) cloudflare_access: Option<CloudflareAccess>,
    pub(super) bundler: Option<BundlerConfig>,
}

#[derive(Clone)]
pub(super) struct CloudflareAccess {
    pub(super) client_id: String,
    pub(super) client_secret: String,
}

#[derive(Clone)]
pub(super) struct BundlerConfig {
    pub(super) rpc_url: Url,
    pub(super) cloudflare_access: Option<CloudflareAccess>,
    pub(super) entry_point: Address,
    pub(super) module: Address,
    pub(super) wallet_deployer: Address,
    pub(super) wallet_count: usize,
    pub(super) deploy_concurrency: usize,
    pub(super) nonce_concurrency: usize,
    pub(super) owner_start_index: u32,
    pub(super) user_operation_timeout: Duration,
    pub(super) user_operation_reject_timeout: Duration,
    pub(super) user_operation_poll_interval: Duration,
    pub(super) sponsorship_max_cost: U256,
    pub(super) sponsorship_validity: Duration,
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
        let bundler = bundler_config_from_env(expected_chain_id, cloudflare_access.as_ref())?;

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
            bundler,
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

impl BundlerConfig {
    pub(super) fn rpc_target(&self) -> String {
        rpc_target(&self.rpc_url)
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

fn bundler_config_from_env(
    expected_chain_id: u64,
    chain_cloudflare_access: Option<&CloudflareAccess>,
) -> eyre::Result<Option<BundlerConfig>> {
    let Some(rpc_url) = optional_env("ACCEPTANCE_BUNDLER_RPC_URL") else {
        return Ok(None);
    };

    Ok(Some(BundlerConfig {
        rpc_url: rpc_url
            .parse()
            .wrap_err("failed to parse ERC-4337 bundler RPC URL")?,
        cloudflare_access: bundler_cloudflare_access_from_env(chain_cloudflare_access)?,
        entry_point: parse_optional_address(
            "ACCEPTANCE_4337_ENTRY_POINT",
            expected_chain_id,
            WORLD_CHAIN_DEVNET_ENTRY_POINT_V0_7,
        )?,
        module: parse_optional_address(
            "ACCEPTANCE_4337_MODULE",
            expected_chain_id,
            WORLD_CHAIN_DEVNET_SAFE_4337_MODULE,
        )?,
        wallet_deployer: parse_optional_address(
            "ACCEPTANCE_4337_WALLET_DEPLOYER",
            expected_chain_id,
            WORLD_CHAIN_DEVNET_SAFE_4337_WALLET_DEPLOYER,
        )?,
        wallet_count: parse_optional_value(
            "ACCEPTANCE_4337_WALLET_COUNT",
            DEFAULT_USER_OPERATION_WALLET_COUNT,
        )?,
        deploy_concurrency: parse_optional_value(
            "ACCEPTANCE_4337_DEPLOY_CONCURRENCY",
            DEFAULT_USER_OPERATION_DEPLOY_CONCURRENCY,
        )?,
        nonce_concurrency: parse_optional_value(
            "ACCEPTANCE_4337_NONCE_CONCURRENCY",
            DEFAULT_USER_OPERATION_NONCE_CONCURRENCY,
        )?,
        owner_start_index: parse_optional_value(
            "ACCEPTANCE_4337_OWNER_START_INDEX",
            DEFAULT_USER_OPERATION_OWNER_START_INDEX,
        )?,
        user_operation_timeout: Duration::from_secs(parse_optional_value(
            "ACCEPTANCE_USEROP_TIMEOUT_SECS",
            DEFAULT_USER_OPERATION_TIMEOUT_SECS,
        )?),
        user_operation_reject_timeout: Duration::from_secs(parse_optional_value(
            "ACCEPTANCE_USEROP_REJECT_TIMEOUT_SECS",
            DEFAULT_USER_OPERATION_REJECT_TIMEOUT_SECS,
        )?),
        user_operation_poll_interval: Duration::from_millis(parse_optional_value(
            "ACCEPTANCE_USEROP_POLL_INTERVAL_MS",
            DEFAULT_USER_OPERATION_POLL_INTERVAL_MS,
        )?),
        sponsorship_max_cost: parse_optional_value_from_str(
            "ACCEPTANCE_4337_SPONSORSHIP_MAX_COST_WEI",
            DEFAULT_USER_OPERATION_SPONSORSHIP_MAX_COST_WEI,
        )?,
        sponsorship_validity: Duration::from_secs(parse_optional_value(
            "ACCEPTANCE_4337_SPONSORSHIP_VALIDITY_SECS",
            DEFAULT_USER_OPERATION_SPONSORSHIP_VALIDITY_SECS,
        )?),
    }))
}

fn bundler_cloudflare_access_from_env(
    chain_cloudflare_access: Option<&CloudflareAccess>,
) -> eyre::Result<Option<CloudflareAccess>> {
    match (
        optional_env("ACCEPTANCE_BUNDLER_CF_ACCESS_CLIENT_ID"),
        optional_env("ACCEPTANCE_BUNDLER_CF_ACCESS_CLIENT_SECRET"),
    ) {
        (Some(client_id), Some(client_secret)) => Ok(Some(CloudflareAccess {
            client_id,
            client_secret,
        })),
        (None, None) => Ok(chain_cloudflare_access.cloned()),
        _ => bail!(
            "ACCEPTANCE_BUNDLER_CF_ACCESS_CLIENT_ID and ACCEPTANCE_BUNDLER_CF_ACCESS_CLIENT_SECRET must be set together"
        ),
    }
}

fn parse_optional_address(
    name: &str,
    expected_chain_id: u64,
    world_chain_devnet_default: Address,
) -> eyre::Result<Address> {
    if let Some(value) = optional_env(name) {
        return parse_value(name, &value);
    }

    if expected_chain_id == WORLD_CHAIN_DEVNET_CHAIN_ID {
        return Ok(world_chain_devnet_default);
    }

    bail!("{name} is required when ACCEPTANCE_BUNDLER_RPC_URL is set outside chain 69420")
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

fn parse_optional_value_from_str<T>(name: &str, default: &str) -> eyre::Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    optional_env(name)
        .unwrap_or_else(|| default.to_string())
        .parse()
        .wrap_err_with(|| format!("failed to parse {name}"))
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

fn rpc_target(rpc_url: &Url) -> String {
    let host = rpc_url.host_str().unwrap_or("<unknown-host>");
    let port = rpc_url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();

    format!("{}://{}{}/...", rpc_url.scheme(), host, port)
}
