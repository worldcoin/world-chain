use clap::{Arg, ArgGroup, Command, builder::TypedValueParser, error::ErrorKind};
use ed25519_dalek::{SigningKey, VerifyingKey};
use hex::FromHex;
use reth_network_peers::TrustedPeer;
use std::{ffi::OsStr, path::PathBuf};
use world_chain_signer::LocalPbhSigner;

use crate::cli::p2p::FanoutArgs;

/// Default number of flashblocks sentries a client maintains as trusted RLPx peers.
///
/// In non critical scenarios, relying on a single sentry to supply flashblocks
/// should be adequate. Users running critical infrastructure may want 2.
pub const DEFAULT_MAX_SENTRY_CONNECTIONS: usize = 1;

/// Parameters for pbh builder configuration
#[derive(Debug, Clone, PartialEq, clap::Args)]
#[command(next_help_heading = "Block Builder", group=ArgGroup::new("pbh.signer").args(["private_key","builder.signer-config"]).multiple(false))]
pub struct BuilderArgs {
    #[arg(
        long = "builder.enabled",
        id = "builder.enabled",
        requires = "pbh.signer",
        required = false
    )]
    pub enabled: bool,

    /// Private key for the builder
    /// used to update PBH nullifiers.
    #[arg(
        long = "builder.private-key",
        alias = "builder.private_key",
        env = "BUILDER_PRIVATE_KEY",
        hide_env_values = true,
        value_parser = LocalPbhKeyParser,
        requires = "builder.enabled",
        conflicts_with = "builder.signer-config"
    )]
    pub private_key: Option<LocalPbhSigner>,

    /// PBH remote signer JSON configuration, including mTLS and expected address.
    #[arg(
        long = "builder.signer-config",
        id = "builder.signer-config",
        env = "BUILDER_SIGNER_CONFIG",
        requires = "builder.enabled",
        conflicts_with = "private_key"
    )]
    pub signer_config: Option<PathBuf>,

    /// Maximum cumulative uncompressed (EIP-2718 encoded) block size in bytes
    #[arg(
        long = "builder.block-uncompressed-size-limit",
        env = "BUILDER_BLOCK_UNCOMPRESSED_SIZE_LIMIT"
    )]
    pub block_uncompressed_size_limit: Option<u64>,
}

#[derive(Clone)]
struct LocalPbhKeyParser;

impl TypedValueParser for LocalPbhKeyParser {
    type Value = LocalPbhSigner;

    fn parse_ref(
        &self,
        cmd: &Command,
        _: Option<&Arg>,
        value: &OsStr,
    ) -> Result<Self::Value, clap::Error> {
        value
            .to_str()
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| {
                clap::Error::raw(ErrorKind::ValueValidation, "invalid local PBH private key")
                    .with_cmd(cmd)
            })
    }
}

impl BuilderArgs {
    pub fn wallet(&self) -> eyre::Result<Option<world_chain_signer::PbhWallet>> {
        use world_chain_signer::{PbhSigner, PbhWallet, bridge::PbhBridge, config::PbhConfig};
        match (&self.signer_config, &self.private_key) {
            (Some(_), Some(_)) => {
                eyre::eyre::bail!("remote PBH signing conflicts with a local private key");
            }
            (Some(path), None) => {
                eyre::eyre::ensure!(
                    self.enabled,
                    "remote PBH signing requires --builder.enabled"
                );
                Ok(Some(PbhWallet::Remote(PbhBridge::new(PbhSigner::new(
                    PbhConfig::load(path)?,
                )?))))
            }
            (None, key) => {
                eyre::eyre::ensure!(
                    !self.enabled || key.is_some(),
                    "enabled builder requires an explicit local key or remote signer"
                );
                Ok(key.clone().map(PbhWallet::Local))
            }
        }
    }
}

/// Flashblocks configuration
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[command(
    next_help_heading = "Flashblocks",
    group = ArgGroup::new("authorizer").multiple(false),
    group = ArgGroup::new("flashblocks.publisher").args(["builder_sk", "flashblocks.signer-config"]).multiple(false)
)]
#[group(id = "flashblocks_presence", requires = "flashblocks.enabled")]
pub struct FlashblocksArgs {
    #[arg(
        long = "flashblocks.enabled",
        id = "flashblocks.enabled",
        group = "flashblocks_presence",
        required = false
    )]
    pub enabled: bool,

    /// Authorizer verifying key
    /// used to verify flashblock authenticity.
    #[arg(
        long = "flashblocks.authorizer-vk",
        alias = "flashblocks.authorizer_vk",
        env = "FLASHBLOCKS_AUTHORIZER_VK",
        group = "authorizer",
        value_parser = parse_vk,
        required = false,
    )]
    pub authorizer_vk: Option<VerifyingKey>,

    /// Flashblocks signing key
    /// used to sign authorized flashblocks payloads.
    #[arg(
        long = "flashblocks.builder-sk",
        alias = "flashblocks.builder_sk",
        env = "FLASHBLOCKS_BUILDER_SK",
        required = false,
        value_parser = parse_sk,
    )]
    pub builder_sk: Option<SigningKey>,

    /// Remote publisher JSON configuration, including mTLS and expected key.
    #[arg(long = "flashblocks.signer-config", id = "flashblocks.signer-config", env = "FLASHBLOCKS_SIGNER_CONFIG", requires = "flashblocks.enabled", conflicts_with_all = ["builder_sk", "override_authorizer_sk", "force_publish"])]
    pub signer_config: Option<PathBuf>,

    /// Remote in-process grant signer JSON, with an independently pinned authorizer key.
    #[arg(long = "flashblocks.authorizer-signer-config", id = "flashblocks.authorizer-signer-config", env = "FLASHBLOCKS_AUTHORIZER_SIGNER_CONFIG", requires_all = ["flashblocks.signer-config", "authorizer_vk"], conflicts_with_all = ["override_authorizer_sk", "force_publish"])]
    pub authorizer_signer_config: Option<PathBuf>,

    /// Override incoming authorizations from rollup boost.
    #[arg(
        long = "flashblocks.override-authorizer-sk",
        alias = "flashblocks.override_authorizer_sk",
        env = "FLASHBLOCKS_OVERRIDE_AUTHORIZER_SK",
        group = "authorizer",
        requires = "builder_sk",
        conflicts_with = "authorizer_vk",
        required = false,
        value_parser = parse_sk,
    )]
    pub override_authorizer_sk: Option<SigningKey>,

    /// Publish flashblocks payloads even when an authorization has not been received from rollup boost.
    ///
    /// This should only be used for testing and development purposes.
    #[arg(
        long = "flashblocks.force-publish",
        alias = "flashblocks.force_publish",
        env = "FLASHBLOCKS_FORCE_PUBLISH",
        requires = "override_authorizer_sk",
        required = false
    )]
    pub force_publish: bool,

    /// The interval to publish pre-confirmations
    /// when building a payload in milliseconds.
    #[arg(
        long = "flashblocks.interval",
        env = "FLASHBLOCKS_INTERVAL",
        default_value_t = 200,
        requires = "flashblocks.publisher"
    )]
    pub flashblocks_interval: u64,

    /// Interval at which the block builder
    /// should re-commit to the transaction pool when building a payload.
    ///
    /// In milliseconds.
    #[arg(
        long = "flashblocks.recommit-interval",
        alias = "flashblocks.recommit_interval",
        env = "FLASHBLOCKS_RECOMMIT_INTERVAL",
        default_value_t = 200,
        requires = "flashblocks.publisher"
    )]
    pub recommit_interval: u64,

    /// Enables flashblocks access list support.
    ///
    /// Will create access lists when building flashblocks payloads.
    /// and will use access lists for parallel transaction execution when verifying
    /// flashblocks payloads.
    #[arg(
        long = "flashblocks.access-list",
        alias = "flashblocks.access_list",
        env = "FLASHBLOCKS_ACCESS_LIST",
        default_value_t = false
    )]
    pub access_list: bool,

    /// Store accepted flashblocks to a separate libmdbx database.
    #[arg(
        long = "flashblocks.store",
        id = "flashblocks.store",
        env = "FLASHBLOCKS_STORE",
        requires = "flashblocks.enabled",
        default_value_t = false
    )]
    pub store: bool,

    /// Path to the libmdbx database directory used by --flashblocks.store.
    ///
    /// Defaults to <datadir>/flashblocks/flashblocks.mdbx.
    #[arg(
        long = "flashblocks.store-path",
        alias = "flashblocks.store_path",
        env = "FLASHBLOCKS_STORE_PATH",
        requires = "flashblocks.store",
        required = false
    )]
    pub store_path: Option<PathBuf>,

    /// Candidate flashblocks sentries and discovery bootnodes.
    ///
    /// By default, clients deterministically select
    /// `--flashblocks.max-sentry-connections` peers from this pool. The selection is stable for a
    /// persisted P2P identity and evenly distributes clients across the pool. Every candidate is
    /// retained as a UDP discovery bootnode, including sentries that are not selected for RLPx.
    #[arg(
        long = "flashblocks.sentry-peers",
        alias = "flashblocks.sentry_peers",
        env = "FLASHBLOCKS_SENTRY_PEERS",
        value_delimiter = ',',
        value_name = "ENODE",
        required = false
    )]
    pub sentry_peers: Vec<TrustedPeer>,

    /// Maximum number of candidate flashblocks sentries maintained as trusted RLPx peers.
    ///
    /// Set this to at least the sentry pool size for builders that must connect to every sentry.
    #[arg(
        long = "flashblocks.max-sentry-connections",
        alias = "flashblocks.max_sentry_connections",
        env = "FLASHBLOCKS_MAX_SENTRY_CONNECTIONS",
        default_value_t = DEFAULT_MAX_SENTRY_CONNECTIONS
    )]
    pub max_sentry_connections: usize,

    #[command(flatten)]
    pub fanout: FanoutArgs,
}

pub fn parse_sk(s: &str) -> eyre::Result<SigningKey> {
    let bytes = <[u8; 32]>::from_hex(s.trim())?;
    Ok(SigningKey::from_bytes(&bytes))
}

pub fn parse_vk(s: &str) -> eyre::Result<VerifyingKey> {
    let bytes = <[u8; 32]>::from_hex(s.trim())?;
    Ok(VerifyingKey::from_bytes(&bytes)?)
}

pub fn parse_trusted_peer(s: &str) -> eyre::Result<Vec<TrustedPeer>> {
    s.split(',')
        .map(|enode| {
            enode.parse().map_err(|err| {
                eyre::Report::msg(format!("invalid flashblocks sentry '{}': {}", enode, err))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct BuilderCommandParser {
        #[command(flatten)]
        builder: BuilderArgs,
    }

    #[test]
    fn builder_key_formats_preserve_wallet_address() {
        let key = format!("{:064x}", 1);
        let prefixed = format!("0x{key}");
        for flag in ["--builder.private-key", "--builder.private_key"] {
            for input in [&key, &prefixed] {
                let args =
                    BuilderCommandParser::try_parse_from(["bin", "--builder.enabled", flag, input])
                        .expect("existing public test key format");
                assert_eq!(
                    args.builder
                        .wallet()
                        .expect("valid local signer configuration")
                        .expect("explicit local signer")
                        .address(),
                    "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf"
                        .parse::<alloy_primitives::Address>()
                        .expect("public test address")
                );
            }
        }
    }

    #[test]
    fn builder_without_signer_requires_credentials_when_enabled() {
        let mut args = BuilderCommandParser::parse_from(["bin"]);
        assert!(
            args.builder
                .wallet()
                .expect("non-builder configuration")
                .is_none()
        );
        args.builder.enabled = true;
        assert!(args.builder.wallet().is_err());
    }

    #[test]
    fn builder_rejects_invalid_signing_keys() {
        for input in [
            "00".repeat(32),
            "ff".repeat(32),
            "01".repeat(33),
            "0x01".to_owned(),
            "not-a-key".to_owned(),
        ] {
            let error = BuilderCommandParser::try_parse_from([
                "bin",
                "--builder.enabled",
                "--builder.private-key",
                &input,
            ])
            .expect_err("invalid PBH private key");
            assert!(!error.to_string().contains(&input));
        }
    }

    #[derive(Debug, Parser)]
    struct CommandParser {
        #[command(flatten)]
        flashblocks: FlashblocksArgs,
    }

    #[test]
    fn flashblocks_override_authorizer() {
        let flashblocks = FlashblocksArgs {
            signer_config: None,
            authorizer_signer_config: None,
            enabled: true,
            override_authorizer_sk: Some(SigningKey::from_bytes(&[0; 32])),
            authorizer_vk: None,
            builder_sk: Some(SigningKey::from_bytes(&[0; 32])),
            force_publish: false,
            recommit_interval: 200,
            flashblocks_interval: 200,
            access_list: true,
            store: false,
            store_path: None,
            sentry_peers: Vec::new(),
            max_sentry_connections: DEFAULT_MAX_SENTRY_CONNECTIONS,
            fanout: FanoutArgs::default(),
        };

        let args = CommandParser::parse_from([
            "bin",
            "--flashblocks.enabled",
            "--flashblocks.override-authorizer-sk",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "--flashblocks.access-list",
            "--flashblocks.builder-sk",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "--flashblocks.interval",
            "200",
            "--flashblocks.recommit-interval",
            "200",
        ]);

        assert_eq!(args.flashblocks, flashblocks);
    }

    #[test]
    fn flashblocks_authorizer() {
        let flashblocks = FlashblocksArgs {
            signer_config: None,
            authorizer_signer_config: None,
            enabled: true,
            override_authorizer_sk: None,
            authorizer_vk: Some(VerifyingKey::from_bytes(&[0; 32]).unwrap()),
            builder_sk: None,
            force_publish: false,
            recommit_interval: 200,
            flashblocks_interval: 200,
            access_list: false,
            store: false,
            store_path: None,
            sentry_peers: Vec::new(),
            max_sentry_connections: DEFAULT_MAX_SENTRY_CONNECTIONS,
            fanout: FanoutArgs::default(),
        };

        let args = CommandParser::parse_from([
            "bin",
            "--flashblocks.enabled",
            "--flashblocks.authorizer-vk",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ]);

        assert_eq!(args.flashblocks, flashblocks);
    }

    #[test]
    fn flashblocks_neither() {
        CommandParser::try_parse_from(["bin", "--flashblocks.enabled"]).unwrap();
    }

    #[test]
    fn flashblocks_both() {
        CommandParser::try_parse_from([
            "bin",
            "--flashblocks.enabled",
            "--flashblocks.override-authorizer",
            "--flashblocks.authorizer-vk",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ])
        .unwrap_err();
    }

    #[test]
    fn flashblocks_store_requires_flashblocks_enabled() {
        CommandParser::try_parse_from(["bin", "--flashblocks.store"]).unwrap_err();
    }

    #[test]
    fn flashblocks_sentry_connection_policy() {
        let args = CommandParser::parse_from([
            "bin",
            "--flashblocks.enabled",
            "--flashblocks.max-sentry-connections",
            "4",
        ]);

        assert_eq!(args.flashblocks.max_sentry_connections, 4);
    }
}
