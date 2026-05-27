//! Configuration for the OP Proposer ExEx.
//!
//! Mirrors `op-proposer/flags/flags.go` and `op-proposer/proposer/config.go`,
//! pared down to the single-chain (pre-interop) DGF proposer.

use std::{path::PathBuf, time::Duration};

use alloy_primitives::Address;
use clap::Args;
use thiserror::Error;

/// CLI configuration for the OP Proposer ExEx, parsed via clap.
///
/// Flag names and semantics mirror `op-proposer` upstream.
#[derive(Debug, Clone, Args)]
#[command(next_help_heading = "OP Proposer ExEx")]
pub struct ProposerCliArgs {
    /// Enable the OP Proposer ExEx.
    #[arg(
        long = "proposer.enabled",
        env = "OP_PROPOSER_ENABLED",
        default_value_t = false
    )]
    pub enabled: bool,

    /// HTTP provider URL for L1.
    #[arg(long = "proposer.l1-eth-rpc", env = "OP_PROPOSER_L1_ETH_RPC")]
    pub l1_eth_rpc: Option<String>,

    /// Optional HTTP provider URL for a remote `op-node` rollup RPC.
    ///
    /// When set, the proposer will read output roots from this endpoint
    /// instead of computing them locally. A comma-separated list enables
    /// active failover (matches the Go behaviour).
    #[arg(long = "proposer.rollup-rpc", env = "OP_PROPOSER_ROLLUP_RPC")]
    pub rollup_rpc: Option<String>,

    /// Address of the `DisputeGameFactory` contract.
    #[arg(
        long = "proposer.game-factory-address",
        env = "OP_PROPOSER_GAME_FACTORY_ADDRESS"
    )]
    pub game_factory_address: Option<Address>,

    /// Interval between checks for whether to load and submit a new proposal.
    #[arg(
        long = "proposer.poll-interval",
        env = "OP_PROPOSER_POLL_INTERVAL",
        default_value = "12s",
        value_parser = parse_duration
    )]
    pub poll_interval: Duration,

    /// Interval between submitting L2 output proposals.
    #[arg(
        long = "proposer.proposal-interval",
        env = "OP_PROPOSER_PROPOSAL_INTERVAL",
        default_value = "1h",
        value_parser = parse_duration
    )]
    pub proposal_interval: Duration,

    /// Allow proposals derived from non-finalized L1 data.
    #[arg(
        long = "proposer.allow-non-finalized",
        env = "OP_PROPOSER_ALLOW_NON_FINALIZED",
        default_value_t = false
    )]
    pub allow_non_finalized: bool,

    /// Dispute game type.
    #[arg(
        long = "proposer.game-type",
        env = "OP_PROPOSER_GAME_TYPE",
        default_value_t = 0u32
    )]
    pub game_type: u32,

    /// Active sequencer check duration (only used with `--proposer.rollup-rpc`).
    #[arg(
        long = "proposer.active-sequencer-check-duration",
        env = "OP_PROPOSER_ACTIVE_SEQUENCER_CHECK_DURATION",
        default_value = "2m",
        value_parser = parse_duration
    )]
    pub active_sequencer_check_duration: Duration,

    /// Whether to wait for the node to sync to the current L1 tip before
    /// starting the driver loop.
    #[arg(
        long = "proposer.wait-node-sync",
        env = "OP_PROPOSER_WAIT_NODE_SYNC",
        default_value_t = false
    )]
    pub wait_node_sync: bool,

    /// L1 network timeout (per-call).
    #[arg(
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
    #[arg(long = "proposer.private-key", env = "OP_PROPOSER_PRIVATE_KEY")]
    pub private_key: Option<String>,

    /// BIP-39 mnemonic for the L1 proposer signer.
    #[arg(long = "proposer.mnemonic", env = "OP_PROPOSER_MNEMONIC")]
    pub mnemonic: Option<String>,

    /// HD derivation path used with `--proposer.mnemonic`.
    #[arg(
        long = "proposer.hd-path",
        env = "OP_PROPOSER_HD_PATH",
        default_value = "m/44'/60'/0'/0/0"
    )]
    pub hd_path: String,

    /// Interval between wallet-balance metric refreshes.
    #[arg(
        long = "proposer.balance-poll-interval",
        env = "OP_PROPOSER_BALANCE_POLL_INTERVAL",
        default_value = "60s",
        value_parser = parse_duration
    )]
    pub balance_poll_interval: Duration,

    /// Maximum rate-limit retries the L1 transport will attempt before
    /// surfacing an error. `0` disables retries.
    #[arg(
        long = "proposer.rpc-max-retries",
        env = "OP_PROPOSER_RPC_MAX_RETRIES",
        default_value_t = 10u32
    )]
    pub rpc_max_retries: u32,

    /// Initial backoff for L1 rate-limit retries (ms).
    #[arg(
        long = "proposer.rpc-initial-backoff-ms",
        env = "OP_PROPOSER_RPC_INITIAL_BACKOFF_MS",
        default_value_t = 500u64
    )]
    pub rpc_initial_backoff_ms: u64,

    /// Compute-units-per-second budget used by alloy's retry backoff layer
    /// to scale waits. Default lifts from world-id-protocol's defaults.
    #[arg(
        long = "proposer.rpc-cups",
        env = "OP_PROPOSER_RPC_CUPS",
        default_value_t = 660u64
    )]
    pub rpc_compute_units_per_second: u64,

    /// Admin RPC bind address.
    #[arg(
        long = "proposer.rpc-addr",
        env = "OP_PROPOSER_RPC_ADDR",
        default_value = "127.0.0.1"
    )]
    pub rpc_addr: String,

    /// Admin RPC bind port. Pass `0` to disable the admin RPC server.
    #[arg(
        long = "proposer.rpc-port",
        env = "OP_PROPOSER_RPC_PORT",
        default_value_t = 0u16
    )]
    pub rpc_port: u16,

    /// Enable admin namespace on the proposer RPC.
    #[arg(
        long = "proposer.rpc-enable-admin",
        env = "OP_PROPOSER_RPC_ENABLE_ADMIN",
        default_value_t = true
    )]
    pub rpc_enable_admin: bool,

    /// Directory for proposer persistent state (MDBX).
    #[arg(long = "proposer.datadir", env = "OP_PROPOSER_DATADIR")]
    pub datadir: Option<PathBuf>,
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
#[derive(Debug, Clone)]
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
