//! Configuration for the OP Proposer ExEx.
//!
//! Mirrors:
//! - [`op-proposer/flags/flags.go`][flags] — CLI flag definitions.
//! - [`op-proposer/proposer/config.go`][config] — `CLIConfig` + validation.
//!
//! Pinned to tag `op-proposer/v1.16.3-rc.1`. Pared down to the single-chain
//! (pre-interop) DGF proposer.
//!
//! [flags]:
//!     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/flags/flags.go
//! [config]:
//!     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/proposer/config.go

use std::{path::PathBuf, time::Duration};

use alloy_primitives::Address;
use clap::Args;
use thiserror::Error;

/// CLI configuration for the OP Proposer ExEx, parsed via clap.
///
/// Flag names and semantics mirror `op-proposer` upstream.
///
/// The `Default` impl mirrors the clap `default_value*` attributes so callers
/// can construct disabled-by-default args in tests without going through
/// `try_parse_from`.
#[derive(Clone, Args)]
#[command(next_help_heading = "OP Proposer ExEx")]
pub struct ProposerCliArgs {
    /// Enable the OP Proposer ExEx.
    #[arg(
        id = "proposer_enabled",
        long = "proposer.enabled",
        env = "OP_PROPOSER_ENABLED",
        default_value_t = false
    )]
    pub enabled: bool,

    /// HTTP provider URL for L1.
    #[arg(
        id = "proposer_l1_eth_rpc",
        long = "proposer.l1-eth-rpc",
        env = "OP_PROPOSER_L1_ETH_RPC"
    )]
    pub l1_eth_rpc: Option<String>,

    /// Optional HTTP provider URL for a remote `op-node` rollup RPC.
    ///
    /// When set, the proposer will read output roots from this endpoint
    /// instead of computing them locally. A comma-separated list enables
    /// active failover (matches the Go behaviour).
    #[arg(
        id = "proposer_rollup_rpc",
        long = "proposer.rollup-rpc",
        env = "OP_PROPOSER_ROLLUP_RPC"
    )]
    pub rollup_rpc: Option<String>,

    /// Address of the `DisputeGameFactory` contract.
    #[arg(
        id = "proposer_game_factory_address",
        long = "proposer.game-factory-address",
        env = "OP_PROPOSER_GAME_FACTORY_ADDRESS"
    )]
    pub game_factory_address: Option<Address>,

    /// Interval between checks for whether to load and submit a new proposal.
    #[arg(
        id = "proposer_poll_interval",
        long = "proposer.poll-interval",
        env = "OP_PROPOSER_POLL_INTERVAL",
        default_value = "12s",
        value_parser = parse_duration
    )]
    pub poll_interval: Duration,

    /// Interval between submitting L2 output proposals.
    #[arg(
        id = "proposer_proposal_interval",
        long = "proposer.proposal-interval",
        env = "OP_PROPOSER_PROPOSAL_INTERVAL",
        default_value = "1h",
        value_parser = parse_duration
    )]
    pub proposal_interval: Duration,

    /// Allow proposals derived from non-finalized L1 data.
    #[arg(
        id = "proposer_allow_non_finalized",
        long = "proposer.allow-non-finalized",
        env = "OP_PROPOSER_ALLOW_NON_FINALIZED",
        default_value_t = false
    )]
    pub allow_non_finalized: bool,

    /// Dispute game type.
    #[arg(
        id = "proposer_game_type",
        long = "proposer.game-type",
        env = "OP_PROPOSER_GAME_TYPE",
        default_value_t = 0u32
    )]
    pub game_type: u32,

    /// Active sequencer check duration (only used with `--proposer.rollup-rpc`).
    #[arg(
        id = "proposer_active_sequencer_check_duration",
        long = "proposer.active-sequencer-check-duration",
        env = "OP_PROPOSER_ACTIVE_SEQUENCER_CHECK_DURATION",
        default_value = "2m",
        value_parser = parse_duration
    )]
    pub active_sequencer_check_duration: Duration,

    /// Whether to wait for the node to sync to the current L1 tip before
    /// starting the driver loop.
    #[arg(
        id = "proposer_wait_node_sync",
        long = "proposer.wait-node-sync",
        env = "OP_PROPOSER_WAIT_NODE_SYNC",
        default_value_t = false
    )]
    pub wait_node_sync: bool,

    /// L1 network timeout (per-call).
    #[arg(
        id = "proposer_network_timeout",
        long = "proposer.network-timeout",
        env = "OP_PROPOSER_NETWORK_TIMEOUT",
        default_value = "10s",
        value_parser = parse_duration
    )]
    pub network_timeout: Duration,

    /// Hex-encoded private key of the L1 proposer EOA.
    ///
    /// Mutually exclusive with `--proposer.mnemonic`. Either is required when
    /// the proposer is enabled.
    #[arg(
        id = "proposer_private_key",
        long = "proposer.private-key",
        env = "OP_PROPOSER_PRIVATE_KEY"
    )]
    pub private_key: Option<String>,

    /// BIP-39 mnemonic for the L1 proposer signer.
    #[arg(
        id = "proposer_mnemonic",
        long = "proposer.mnemonic",
        env = "OP_PROPOSER_MNEMONIC"
    )]
    pub mnemonic: Option<String>,

    /// HD derivation path used with `--proposer.mnemonic`.
    #[arg(
        id = "proposer_hd_path",
        long = "proposer.hd-path",
        env = "OP_PROPOSER_HD_PATH",
        default_value = "m/44'/60'/0'/0/0"
    )]
    pub hd_path: String,

    /// Interval between wallet-balance metric refreshes.
    #[arg(
        id = "proposer_balance_poll_interval",
        long = "proposer.balance-poll-interval",
        env = "OP_PROPOSER_BALANCE_POLL_INTERVAL",
        default_value = "60s",
        value_parser = parse_duration
    )]
    pub balance_poll_interval: Duration,

    /// Maximum rate-limit retries the L1 transport will attempt before
    /// surfacing an error. `0` disables retries.
    #[arg(
        id = "proposer_rpc_max_retries",
        long = "proposer.rpc-max-retries",
        env = "OP_PROPOSER_RPC_MAX_RETRIES",
        default_value_t = 10u32
    )]
    pub rpc_max_retries: u32,

    /// Initial backoff for L1 rate-limit retries (ms).
    #[arg(
        id = "proposer_rpc_initial_backoff_ms",
        long = "proposer.rpc-initial-backoff-ms",
        env = "OP_PROPOSER_RPC_INITIAL_BACKOFF_MS",
        default_value_t = 500u64
    )]
    pub rpc_initial_backoff_ms: u64,

    /// Compute-units-per-second budget used by alloy's retry backoff layer
    /// to scale waits.
    #[arg(
        id = "proposer_rpc_cups",
        long = "proposer.rpc-cups",
        env = "OP_PROPOSER_RPC_CUPS",
        default_value_t = 660u64
    )]
    pub rpc_compute_units_per_second: u64,

    /// Admin RPC bind address.
    #[arg(
        id = "proposer_rpc_addr",
        long = "proposer.rpc-addr",
        env = "OP_PROPOSER_RPC_ADDR",
        default_value = "127.0.0.1"
    )]
    pub rpc_addr: String,

    /// Admin RPC bind port. Pass `0` to disable the admin RPC server.
    #[arg(
        id = "proposer_rpc_port",
        long = "proposer.rpc-port",
        env = "OP_PROPOSER_RPC_PORT",
        default_value_t = 0u16
    )]
    pub rpc_port: u16,

    /// Enable admin namespace on the proposer RPC.
    #[arg(
        id = "proposer_rpc_enable_admin",
        long = "proposer.rpc-enable-admin",
        env = "OP_PROPOSER_RPC_ENABLE_ADMIN",
        default_value_t = true
    )]
    pub rpc_enable_admin: bool,

    /// Directory for proposer persistent state (MDBX).
    #[arg(
        id = "proposer_datadir",
        long = "proposer.datadir",
        env = "OP_PROPOSER_DATADIR"
    )]
    pub datadir: Option<PathBuf>,
}

/// Manual `Debug` impl: redacts `private_key` and `mnemonic`.
///
/// Mirrors the redaction on [`ProposerConfig`] so secrets can't leak via
/// `tracing::error!(?args, ...)` on the CLI-parsed args before they've been
/// translated into a [`ProposerConfig`].
impl std::fmt::Debug for ProposerCliArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProposerCliArgs")
            .field("enabled", &self.enabled)
            .field("l1_eth_rpc", &self.l1_eth_rpc)
            .field("rollup_rpc", &self.rollup_rpc)
            .field("game_factory_address", &self.game_factory_address)
            .field("poll_interval", &self.poll_interval)
            .field("proposal_interval", &self.proposal_interval)
            .field("allow_non_finalized", &self.allow_non_finalized)
            .field("game_type", &self.game_type)
            .field(
                "active_sequencer_check_duration",
                &self.active_sequencer_check_duration,
            )
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

impl Default for ProposerCliArgs {
    fn default() -> Self {
        Self {
            enabled: false,
            l1_eth_rpc: None,
            rollup_rpc: None,
            game_factory_address: None,
            poll_interval: Duration::from_secs(12),
            proposal_interval: Duration::from_secs(3600),
            allow_non_finalized: false,
            game_type: 0,
            active_sequencer_check_duration: Duration::from_secs(120),
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

/// Minimal humantime-style duration parser (`12s`, `1h`, `500ms`, `2m`).
fn parse_humantime(raw: &str) -> Result<Duration, ProposerConfigError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ProposerConfigError::InvalidDuration(raw.to_string()));
    }
    if let Ok(secs) = trimmed.parse::<u64>() {
        return Ok(Duration::from_secs(secs));
    }
    let (number, suffix) = trimmed.split_at(
        trimmed
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .ok_or_else(|| ProposerConfigError::InvalidDuration(raw.to_string()))?,
    );
    let value: f64 = number
        .parse()
        .map_err(|_| ProposerConfigError::InvalidDuration(raw.to_string()))?;
    let nanos = match suffix {
        "ns" => value,
        "us" | "µs" => value * 1_000.0,
        "ms" => value * 1_000_000.0,
        "s" => value * 1_000_000_000.0,
        "m" => value * 60.0 * 1_000_000_000.0,
        "h" => value * 3_600.0 * 1_000_000_000.0,
        _ => return Err(ProposerConfigError::InvalidDuration(raw.to_string())),
    };
    if !nanos.is_finite() || nanos < 0.0 {
        return Err(ProposerConfigError::InvalidDuration(raw.to_string()));
    }
    Ok(Duration::from_nanos(nanos as u64))
}

/// Runtime configuration consumed by the proposer service.
///
/// `Debug` is implemented manually below to redact `private_key` and
/// `mnemonic` so accidental `tracing::error!(?cfg, ...)` calls (or panic
/// messages) can't leak the proposer EOA secrets into logs.
#[derive(Clone)]
pub struct ProposerConfig {
    pub l1_eth_rpcs: Vec<String>,
    pub rollup_rpcs: Vec<String>,

    pub game_factory_address: Address,
    pub game_type: u32,

    pub poll_interval: Duration,
    pub proposal_interval: Duration,
    pub network_timeout: Duration,
    pub active_sequencer_check_duration: Duration,
    pub balance_poll_interval: Duration,

    pub allow_non_finalized: bool,
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

/// Manual `Debug` impl: redacts `private_key` and `mnemonic`.
impl std::fmt::Debug for ProposerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProposerConfig")
            .field("l1_eth_rpcs", &self.l1_eth_rpcs)
            .field("rollup_rpcs", &self.rollup_rpcs)
            .field("game_factory_address", &self.game_factory_address)
            .field("game_type", &self.game_type)
            .field("poll_interval", &self.poll_interval)
            .field("proposal_interval", &self.proposal_interval)
            .field("network_timeout", &self.network_timeout)
            .field(
                "active_sequencer_check_duration",
                &self.active_sequencer_check_duration,
            )
            .field("balance_poll_interval", &self.balance_poll_interval)
            .field("allow_non_finalized", &self.allow_non_finalized)
            .field("wait_node_sync", &self.wait_node_sync)
            .field("private_key", &redacted(&self.private_key))
            .field("mnemonic", &redacted(&self.mnemonic))
            .field("hd_path", &self.hd_path)
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

fn redacted(secret: &Option<String>) -> &'static str {
    match secret {
        Some(_) => "<redacted>",
        None => "None",
    }
}

/// Mirrors: `(*CLIConfig).Check` + `NewConfig` in
/// [config.go L87–L160][src]. Validation differs from upstream in two ways:
///
/// * The interop game-type matrix (`preInteropGameTypes` / `postInteropGameTypes`)
///   is intentionally not enforced — interop is unsupported.
/// * The "exactly one signer" check is added (upstream pulls signer config
///   out of `txmgr.CLIConfig` which lives in a separate package).
///
/// [src]:
///     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/proposer/config.go#L87-L160
impl ProposerCliArgs {
    /// Translate CLI args into a validated [`ProposerConfig`].
    ///
    /// Mirrors `CLIConfig.Check()` from `op-proposer/proposer/config.go`,
    /// minus the interop game-type matrix.
    pub fn into_config(
        self,
        fallback_datadir: PathBuf,
    ) -> Result<ProposerConfig, ProposerConfigError> {
        let l1_eth_rpcs: Vec<String> = self
            .l1_eth_rpc
            .as_deref()
            .ok_or(ProposerConfigError::MissingL1EthRpc)?
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if l1_eth_rpcs.is_empty() {
            return Err(ProposerConfigError::MissingL1EthRpc);
        }
        let game_factory_address = self
            .game_factory_address
            .ok_or(ProposerConfigError::MissingGameFactory)?;

        if self.proposal_interval.is_zero() {
            return Err(ProposerConfigError::ZeroProposalInterval);
        }

        let rollup_rpcs: Vec<String> = self
            .rollup_rpc
            .as_deref()
            .map(|s| {
                s.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        if self.private_key.is_none() && self.mnemonic.is_none() {
            return Err(ProposerConfigError::MissingSigner);
        }
        if self.private_key.is_some() && self.mnemonic.is_some() {
            return Err(ProposerConfigError::ConflictingSigner);
        }

        let datadir = self.datadir.unwrap_or(fallback_datadir);

        Ok(ProposerConfig {
            l1_eth_rpcs,
            rollup_rpcs,
            game_factory_address,
            game_type: self.game_type,
            poll_interval: self.poll_interval,
            proposal_interval: self.proposal_interval,
            network_timeout: self.network_timeout,
            active_sequencer_check_duration: self.active_sequencer_check_duration,
            balance_poll_interval: self.balance_poll_interval,
            allow_non_finalized: self.allow_non_finalized,
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
pub enum ProposerConfigError {
    #[error("missing L1 RPC (--proposer.l1-eth-rpc)")]
    MissingL1EthRpc,
    #[error("missing DisputeGameFactory address (--proposer.game-factory-address)")]
    MissingGameFactory,
    #[error("--proposer.proposal-interval must be non-zero when DGF is configured")]
    ZeroProposalInterval,
    #[error("missing signer: provide either --proposer.private-key or --proposer.mnemonic")]
    MissingSigner,
    #[error(
        "conflicting signer: provide only one of --proposer.private-key / --proposer.mnemonic"
    )]
    ConflictingSigner,
    #[error("invalid duration string: {0}")]
    InvalidDuration(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_humantime() {
        assert_eq!(parse_humantime("12s").unwrap(), Duration::from_secs(12));
        assert_eq!(parse_humantime("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(
            parse_humantime("500ms").unwrap(),
            Duration::from_millis(500)
        );
        assert_eq!(parse_humantime("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_humantime("7").unwrap(), Duration::from_secs(7));
        assert!(parse_humantime("nope").is_err());
    }
}
