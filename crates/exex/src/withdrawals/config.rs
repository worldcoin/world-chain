//! Configuration for the World Chain Relayer ExEx (cacher subset).
//!
//! Mirrors the [`ProposerCliArgs`](crate::proposer::config::ProposerCliArgs) clap pattern.
//! Only the flags the **cacher** consumes today are wired up here:
//!
//! * `--relayer.enabled`   (env `WC_RELAYER_ENABLED`, default `false`)
//! * `--relayer.cache-only` (env `WC_RELAYER_CACHE_ONLY`, default `false`)
//! * `--relayer.datadir`   (env `WC_RELAYER_DATADIR`, default `<datadir>/wc-relayer`)
//!
//! The remaining `--relayer.*` flags —
//! `--relayer.l1-eth-rpc`, `--relayer.portal-address`,
//! `--relayer.game-factory-address`, `--relayer.game-type`,
//! `--relayer.poll-interval`, the signer flags (`--relayer.private-key` /
//! `--relayer.mnemonic` / `--relayer.hd-path`), `--relayer.network-timeout`,
//! and the admin-RPC flags — arrive with the relayer driver. They are
//! intentionally omitted here so the cacher build stays free of dead code
//! under the workspace's `-D warnings` lint policy.

use std::path::PathBuf;

use clap::Args;
use thiserror::Error;

/// CLI configuration for the World Chain Relayer ExEx (cacher subset).
///
/// The `Default` impl mirrors the clap `default_value*` attributes so callers
/// can construct disabled-by-default args in tests without going through
/// `try_parse_from`.
#[derive(Clone, Debug, Args)]
#[command(next_help_heading = "World Chain Relayer ExEx")]
pub struct RelayerCliArgs {
    /// Enable the withdrawal cacher + relayer ExEx.
    #[arg(
        id = "relayer_enabled",
        long = "relayer.enabled",
        env = "WC_RELAYER_ENABLED",
        default_value_t = false
    )]
    pub enabled: bool,

    /// Run the cacher only; never submit L1 transactions.
    ///
    /// The relayer driver honours this flag once it lands; the cacher's
    /// behaviour is identical either way.
    #[arg(
        id = "relayer_cache_only",
        long = "relayer.cache-only",
        env = "WC_RELAYER_CACHE_ONLY",
        default_value_t = false
    )]
    pub cache_only: bool,

    /// MDBX store location for the withdrawal cache.
    ///
    /// Defaults to `<datadir>/wc-relayer` (resolved in
    /// [`RelayerCliArgs::into_config`]).
    #[arg(
        id = "relayer_datadir",
        long = "relayer.datadir",
        env = "WC_RELAYER_DATADIR"
    )]
    pub datadir: Option<PathBuf>,
}

#[allow(clippy::derivable_impls)]
impl Default for RelayerCliArgs {
    fn default() -> Self {
        // Spelled out (rather than `#[derive(Default)]`) to mirror the
        // explicit clap-default `Default` impl on
        // [`ProposerCliArgs`](crate::proposer::config::ProposerCliArgs), and to keep the
        // defaults co-located with the flag table as more flags land.
        Self {
            enabled: false,
            cache_only: false,
            datadir: None,
        }
    }
}

/// Runtime configuration consumed by the relayer ExEx (cacher subset).
#[derive(Clone, Debug)]
pub struct RelayerConfig {
    /// Run the cacher only; never submit L1 transactions.
    pub cache_only: bool,
    /// Resolved MDBX store location for the withdrawal cache.
    pub datadir: PathBuf,
}

impl RelayerCliArgs {
    /// Translate CLI args into a validated [`RelayerConfig`].
    ///
    /// The cacher needs no L1 endpoint or signer, so the only resolution here
    /// is the datadir fallback. The signer/RPC validation arrives with the
    /// relayer driver.
    pub fn into_config(
        self,
        fallback_datadir: PathBuf,
    ) -> Result<RelayerConfig, RelayerConfigError> {
        let datadir = self.datadir.unwrap_or(fallback_datadir);
        Ok(RelayerConfig {
            cache_only: self.cache_only,
            datadir,
        })
    }
}

#[derive(Debug, Error)]
pub enum RelayerConfigError {
    /// Placeholder for the signer/endpoint validation added with the relayer
    /// driver. Kept so the error seam and the
    /// [`OpProposerError`](crate::error::OpProposerError) `#[from]` wiring
    /// are in place; the cacher never constructs it.
    // Forward-compat seam for the relayer driver; the cacher never builds it.
    #[allow(dead_code)]
    #[error("invalid relayer configuration: {0}")]
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled() {
        let args = RelayerCliArgs::default();
        assert!(!args.enabled);
        assert!(!args.cache_only);
        assert!(args.datadir.is_none());
    }

    #[test]
    fn into_config_uses_fallback_datadir() {
        let args = RelayerCliArgs::default();
        let cfg = args.into_config(PathBuf::from("/tmp/wc-relayer")).unwrap();
        assert_eq!(cfg.datadir, PathBuf::from("/tmp/wc-relayer"));
        assert!(!cfg.cache_only);
    }

    #[test]
    fn into_config_prefers_explicit_datadir() {
        let args = RelayerCliArgs {
            datadir: Some(PathBuf::from("/custom")),
            ..Default::default()
        };
        let cfg = args.into_config(PathBuf::from("/fallback")).unwrap();
        assert_eq!(cfg.datadir, PathBuf::from("/custom"));
    }
}
