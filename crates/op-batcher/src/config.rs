//! Configuration for the OP Batcher ExEx.
//!
//! Mirrors `op-batcher/flags/flags.go` + `op-batcher/batcher/config.go`, pared
//! down to the v1 scope: singular batches, calldata DA, zlib compression. Flag
//! names follow `op-batcher` upstream where they map cleanly.
//!
//! See `crates/op-batcher/BATCHER_SPEC.md` §9 for the full flag table.

use std::{path::PathBuf, time::Duration};

use alloy_primitives::Address;
use clap::Args;
use thiserror::Error;

/// CLI configuration for the OP Batcher ExEx, parsed via clap.
#[derive(Clone, Args)]
#[command(next_help_heading = "OP Batcher ExEx")]
pub struct BatcherCliArgs {
    /// Enable the OP Batcher ExEx.
    #[arg(
        id = "batcher_enabled",
        long = "batcher.enabled",
        env = "OP_BATCHER_ENABLED",
        default_value_t = false
    )]
    pub enabled: bool,

    /// HTTP provider URL(s) for L1 (comma-separated for failover).
    #[arg(
        id = "batcher_l1_eth_rpc",
        long = "batcher.l1-eth-rpc",
        env = "OP_BATCHER_L1_ETH_RPC"
    )]
    pub l1_eth_rpc: Option<String>,

    /// Address of the L1 `BatchInbox` that batch transactions are sent to.
    #[arg(
        id = "batcher_batch_inbox_address",
        long = "batcher.batch-inbox-address",
        env = "OP_BATCHER_BATCH_INBOX_ADDRESS"
    )]
    pub batch_inbox_address: Option<Address>,

    /// Interval between batch-submission polling cycles.
    #[arg(
        id = "batcher_poll_interval",
        long = "batcher.poll-interval",
        env = "OP_BATCHER_POLL_INTERVAL",
        default_value = "6s",
        value_parser = parse_duration
    )]
    pub poll_interval: Duration,

    /// Maximum L1 calldata tx size (bytes). The max frame size is this minus 1.
    #[arg(
        id = "batcher_max_l1_tx_size_bytes",
        long = "batcher.max-l1-tx-size-bytes",
        env = "OP_BATCHER_MAX_L1_TX_SIZE_BYTES",
        default_value_t = 120_000u64
    )]
    pub max_l1_tx_size_bytes: u64,

    /// Maximum number of L1 blocks a channel may remain open before being
    /// force-closed. `0` disables the duration timeout.
    #[arg(
        id = "batcher_max_channel_duration",
        long = "batcher.max-channel-duration",
        env = "OP_BATCHER_MAX_CHANNEL_DURATION",
        default_value_t = 0u64
    )]
    pub max_channel_duration: u64,

    /// Number of L1 blocks subtracted from channel timeout / sequencing window
    /// when computing the close deadline, so the channel lands with margin.
    #[arg(
        id = "batcher_sub_safety_margin",
        long = "batcher.sub-safety-margin",
        env = "OP_BATCHER_SUB_SAFETY_MARGIN",
        default_value_t = 10u64
    )]
    pub sub_safety_margin: u64,

    /// Approximate compression ratio used to decide when a channel is full
    /// (input target = target output size / ratio).
    #[arg(
        id = "batcher_approx_compr_ratio",
        long = "batcher.approx-compr-ratio",
        env = "OP_BATCHER_APPROX_COMPR_RATIO",
        default_value_t = 0.6f64
    )]
    pub approx_compr_ratio: f64,

    /// On-chain channel timeout (L1 blocks). Defaults to the Granite value.
    #[arg(
        id = "batcher_channel_timeout",
        long = "batcher.channel-timeout",
        env = "OP_BATCHER_CHANNEL_TIMEOUT",
        default_value_t = 50u64
    )]
    pub channel_timeout: u64,

    /// Whether to wait for the node to sync before starting the driver loop.
    #[arg(
        id = "batcher_wait_node_sync",
        long = "batcher.wait-node-sync",
        env = "OP_BATCHER_WAIT_NODE_SYNC",
        default_value_t = false
    )]
    pub wait_node_sync: bool,

    /// Allow batching against the unsafe head even when it is not yet safe
    /// (the normal mode). Reserved for future use; always true for v1.
    #[arg(
        id = "batcher_network_timeout",
        long = "batcher.network-timeout",
        env = "OP_BATCHER_NETWORK_TIMEOUT",
        default_value = "10s",
        value_parser = parse_duration
    )]
    pub network_timeout: Duration,

    /// Hex-encoded private key of the L1 batcher EOA (must match
    /// `SystemConfig.batcherHash`). Mutually exclusive with `--batcher.mnemonic`.
    #[arg(
        id = "batcher_private_key",
        long = "batcher.private-key",
        env = "OP_BATCHER_PRIVATE_KEY"
    )]
    pub private_key: Option<String>,

    /// BIP-39 mnemonic for the L1 batcher signer.
    #[arg(
        id = "batcher_mnemonic",
        long = "batcher.mnemonic",
        env = "OP_BATCHER_MNEMONIC"
    )]
    pub mnemonic: Option<String>,

    /// HD derivation path used with `--batcher.mnemonic`.
    #[arg(
        id = "batcher_hd_path",
        long = "batcher.hd-path",
        env = "OP_BATCHER_HD_PATH",
        default_value = "m/44'/60'/0'/0/0"
    )]
    pub hd_path: String,

    /// Interval between wallet-balance metric refreshes.
    #[arg(
        id = "batcher_balance_poll_interval",
        long = "batcher.balance-poll-interval",
        env = "OP_BATCHER_BALANCE_POLL_INTERVAL",
        default_value = "60s",
        value_parser = parse_duration
    )]
    pub balance_poll_interval: Duration,

    /// Maximum rate-limit retries the L1 transport will attempt. `0` disables.
    #[arg(
        id = "batcher_rpc_max_retries",
        long = "batcher.rpc-max-retries",
        env = "OP_BATCHER_RPC_MAX_RETRIES",
        default_value_t = 10u32
    )]
    pub rpc_max_retries: u32,

    /// Initial backoff for L1 rate-limit retries (ms).
    #[arg(
        id = "batcher_rpc_initial_backoff_ms",
        long = "batcher.rpc-initial-backoff-ms",
        env = "OP_BATCHER_RPC_INITIAL_BACKOFF_MS",
        default_value_t = 500u64
    )]
    pub rpc_initial_backoff_ms: u64,

    /// Compute-units-per-second budget used by alloy's retry backoff layer.
    #[arg(
        id = "batcher_rpc_cups",
        long = "batcher.rpc-cups",
        env = "OP_BATCHER_RPC_CUPS",
        default_value_t = 660u64
    )]
    pub rpc_compute_units_per_second: u64,

    /// Admin RPC bind address.
    #[arg(
        id = "batcher_rpc_addr",
        long = "batcher.rpc-addr",
        env = "OP_BATCHER_RPC_ADDR",
        default_value = "127.0.0.1"
    )]
    pub rpc_addr: String,

    /// Admin RPC bind port. Pass `0` to disable the admin RPC server.
    #[arg(
        id = "batcher_rpc_port",
        long = "batcher.rpc-port",
        env = "OP_BATCHER_RPC_PORT",
        default_value_t = 0u16
    )]
    pub rpc_port: u16,

    /// Enable the admin namespace on the batcher RPC.
    #[arg(
        id = "batcher_rpc_enable_admin",
        long = "batcher.rpc-enable-admin",
        env = "OP_BATCHER_RPC_ENABLE_ADMIN",
        default_value_t = true
    )]
    pub rpc_enable_admin: bool,

    /// Directory for batcher persistent state (MDBX).
    #[arg(
        id = "batcher_datadir",
        long = "batcher.datadir",
        env = "OP_BATCHER_DATADIR"
    )]
    pub datadir: Option<PathBuf>,
}

/// Manual `Debug`: redacts `private_key` / `mnemonic`.
impl std::fmt::Debug for BatcherCliArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BatcherCliArgs")
            .field("enabled", &self.enabled)
            .field("l1_eth_rpc", &self.l1_eth_rpc)
            .field("batch_inbox_address", &self.batch_inbox_address)
            .field("poll_interval", &self.poll_interval)
            .field("max_l1_tx_size_bytes", &self.max_l1_tx_size_bytes)
            .field("max_channel_duration", &self.max_channel_duration)
            .field("sub_safety_margin", &self.sub_safety_margin)
            .field("approx_compr_ratio", &self.approx_compr_ratio)
            .field("channel_timeout", &self.channel_timeout)
            .field("wait_node_sync", &self.wait_node_sync)
            .field("network_timeout", &self.network_timeout)
            .field("private_key", &redacted(&self.private_key))
            .field("mnemonic", &redacted(&self.mnemonic))
            .field("hd_path", &self.hd_path)
            .field("balance_poll_interval", &self.balance_poll_interval)
            .field("rpc_max_retries", &self.rpc_max_retries)
            .field("rpc_initial_backoff_ms", &self.rpc_initial_backoff_ms)
            .field(
                "rpc_compute_units_per_second",
                &self.rpc_compute_units_per_second,
            )
            .field("rpc_addr", &self.rpc_addr)
            .field("rpc_port", &self.rpc_port)
            .field("rpc_enable_admin", &self.rpc_enable_admin)
            .field("datadir", &self.datadir)
            .finish()
    }
}

impl Default for BatcherCliArgs {
    fn default() -> Self {
        Self {
            enabled: false,
            l1_eth_rpc: None,
            batch_inbox_address: None,
            poll_interval: Duration::from_secs(6),
            max_l1_tx_size_bytes: 120_000,
            max_channel_duration: 0,
            sub_safety_margin: 10,
            approx_compr_ratio: 0.6,
            channel_timeout: 50,
            wait_node_sync: false,
            network_timeout: Duration::from_secs(10),
            private_key: None,
            mnemonic: None,
            hd_path: "m/44'/60'/0'/0/0".to_string(),
            balance_poll_interval: Duration::from_secs(60),
            rpc_max_retries: 10,
            rpc_initial_backoff_ms: 500,
            rpc_compute_units_per_second: 660,
            rpc_addr: "127.0.0.1".to_string(),
            rpc_port: 0,
            rpc_enable_admin: true,
            datadir: None,
        }
    }
}

fn parse_duration(raw: &str) -> Result<Duration, String> {
    parse_humantime(raw).map_err(|e| e.to_string())
}

/// Minimal humantime-style duration parser (`6s`, `1h`, `500ms`, `2m`).
fn parse_humantime(raw: &str) -> Result<Duration, BatcherConfigError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(BatcherConfigError::InvalidDuration(raw.to_string()));
    }
    if let Ok(secs) = trimmed.parse::<u64>() {
        return Ok(Duration::from_secs(secs));
    }
    let (number, suffix) = trimmed.split_at(
        trimmed
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .ok_or_else(|| BatcherConfigError::InvalidDuration(raw.to_string()))?,
    );
    let value: f64 = number
        .parse()
        .map_err(|_| BatcherConfigError::InvalidDuration(raw.to_string()))?;
    let nanos = match suffix {
        "ns" => value,
        "us" | "µs" => value * 1_000.0,
        "ms" => value * 1_000_000.0,
        "s" => value * 1_000_000_000.0,
        "m" => value * 60.0 * 1_000_000_000.0,
        "h" => value * 3_600.0 * 1_000_000_000.0,
        _ => return Err(BatcherConfigError::InvalidDuration(raw.to_string())),
    };
    if !nanos.is_finite() || nanos < 0.0 {
        return Err(BatcherConfigError::InvalidDuration(raw.to_string()));
    }
    Ok(Duration::from_nanos(nanos as u64))
}

/// Runtime configuration consumed by the batcher service. `Debug` is manual to
/// redact secrets.
#[derive(Clone)]
pub struct BatcherConfig {
    pub l1_eth_rpcs: Vec<String>,
    pub batch_inbox_address: Address,

    pub poll_interval: Duration,
    pub network_timeout: Duration,
    pub balance_poll_interval: Duration,

    pub max_l1_tx_size_bytes: u64,
    pub max_channel_duration: u64,
    pub sub_safety_margin: u64,
    pub approx_compr_ratio: f64,
    pub channel_timeout: u64,

    pub wait_node_sync: bool,

    pub private_key: Option<String>,
    pub mnemonic: Option<String>,
    pub hd_path: String,

    pub rpc_max_retries: u32,
    pub rpc_initial_backoff_ms: u64,
    pub rpc_compute_units_per_second: u64,

    pub rpc_addr: String,
    pub rpc_port: u16,
    pub rpc_enable_admin: bool,

    pub datadir: PathBuf,
}

impl BatcherConfig {
    /// Max frame size = max L1 tx size - 1 (calldata DA).
    pub fn max_frame_size(&self) -> u64 {
        self.max_l1_tx_size_bytes.saturating_sub(1)
    }
}

impl std::fmt::Debug for BatcherConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BatcherConfig")
            .field("l1_eth_rpcs", &self.l1_eth_rpcs)
            .field("batch_inbox_address", &self.batch_inbox_address)
            .field("poll_interval", &self.poll_interval)
            .field("network_timeout", &self.network_timeout)
            .field("balance_poll_interval", &self.balance_poll_interval)
            .field("max_l1_tx_size_bytes", &self.max_l1_tx_size_bytes)
            .field("max_channel_duration", &self.max_channel_duration)
            .field("sub_safety_margin", &self.sub_safety_margin)
            .field("approx_compr_ratio", &self.approx_compr_ratio)
            .field("channel_timeout", &self.channel_timeout)
            .field("wait_node_sync", &self.wait_node_sync)
            .field("private_key", &redacted(&self.private_key))
            .field("mnemonic", &redacted(&self.mnemonic))
            .field("hd_path", &self.hd_path)
            .field("rpc_max_retries", &self.rpc_max_retries)
            .field("rpc_addr", &self.rpc_addr)
            .field("rpc_port", &self.rpc_port)
            .field("rpc_enable_admin", &self.rpc_enable_admin)
            .field("datadir", &self.datadir)
            .finish()
    }
}

fn redacted(secret: &Option<String>) -> &'static str {
    match secret {
        Some(_) => "<redacted>",
        None => "None",
    }
}

impl BatcherCliArgs {
    /// Translate CLI args into a validated [`BatcherConfig`].
    pub fn into_config(
        self,
        fallback_datadir: PathBuf,
    ) -> Result<BatcherConfig, BatcherConfigError> {
        let l1_eth_rpcs: Vec<String> = self
            .l1_eth_rpc
            .as_deref()
            .ok_or(BatcherConfigError::MissingL1EthRpc)?
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if l1_eth_rpcs.is_empty() {
            return Err(BatcherConfigError::MissingL1EthRpc);
        }

        let batch_inbox_address = self
            .batch_inbox_address
            .ok_or(BatcherConfigError::MissingBatchInbox)?;

        if self.private_key.is_none() && self.mnemonic.is_none() {
            return Err(BatcherConfigError::MissingSigner);
        }
        if self.private_key.is_some() && self.mnemonic.is_some() {
            return Err(BatcherConfigError::ConflictingSigner);
        }
        if self.max_l1_tx_size_bytes <= crate::channel_out::FRAME_V0_OVERHEAD_SIZE as u64 {
            return Err(BatcherConfigError::TxSizeTooSmall);
        }
        if !(self.approx_compr_ratio > 0.0 && self.approx_compr_ratio <= 1.0) {
            return Err(BatcherConfigError::InvalidComprRatio);
        }

        let datadir = self.datadir.unwrap_or(fallback_datadir);

        Ok(BatcherConfig {
            l1_eth_rpcs,
            batch_inbox_address,
            poll_interval: self.poll_interval,
            network_timeout: self.network_timeout,
            balance_poll_interval: self.balance_poll_interval,
            max_l1_tx_size_bytes: self.max_l1_tx_size_bytes,
            max_channel_duration: self.max_channel_duration,
            sub_safety_margin: self.sub_safety_margin,
            approx_compr_ratio: self.approx_compr_ratio,
            channel_timeout: self.channel_timeout,
            wait_node_sync: self.wait_node_sync,
            private_key: self.private_key,
            mnemonic: self.mnemonic,
            hd_path: self.hd_path,
            rpc_max_retries: self.rpc_max_retries,
            rpc_initial_backoff_ms: self.rpc_initial_backoff_ms,
            rpc_compute_units_per_second: self.rpc_compute_units_per_second,
            rpc_addr: self.rpc_addr,
            rpc_port: self.rpc_port,
            rpc_enable_admin: self.rpc_enable_admin,
            datadir,
        })
    }
}

#[derive(Debug, Error)]
pub enum BatcherConfigError {
    #[error("missing L1 RPC (--batcher.l1-eth-rpc)")]
    MissingL1EthRpc,
    #[error("missing BatchInbox address (--batcher.batch-inbox-address)")]
    MissingBatchInbox,
    #[error("missing signer: provide either --batcher.private-key or --batcher.mnemonic")]
    MissingSigner,
    #[error("conflicting signer: provide only one of --batcher.private-key / --batcher.mnemonic")]
    ConflictingSigner,
    #[error("--batcher.max-l1-tx-size-bytes is too small to fit the frame overhead")]
    TxSizeTooSmall,
    #[error("--batcher.approx-compr-ratio must be in (0, 1]")]
    InvalidComprRatio,
    #[error("invalid duration string: {0}")]
    InvalidDuration(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_humantime() {
        assert_eq!(parse_humantime("6s").unwrap(), Duration::from_secs(6));
        assert_eq!(parse_humantime("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(
            parse_humantime("500ms").unwrap(),
            Duration::from_millis(500)
        );
        assert!(parse_humantime("nope").is_err());
    }

    #[test]
    fn requires_l1_and_inbox_and_signer() {
        let args = BatcherCliArgs::default();
        assert!(matches!(
            args.into_config(PathBuf::from("/tmp/x")),
            Err(BatcherConfigError::MissingL1EthRpc)
        ));
    }
}
