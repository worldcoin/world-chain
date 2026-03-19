use clap::ArgGroup;
use ed25519_dalek::{SigningKey, VerifyingKey};
use hex::FromHex;
use reth_network_peers::PeerId;

pub const DEFAULT_MAX_SEND_PEERS: usize = 10;
pub const DEFAULT_MAX_RECEIVE_PEERS: usize = 3;
pub const DEFAULT_ROTATION_INTERVAL: u64 = 30;
pub const DEFAULT_SCORE_SAMPLES: i64 = 1000;

/// Flashblocks configuration
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct FanoutArgs {
    /// Override the flashblocks send-set size.
    #[arg(
        long = "flashblocks.max_send_peers",
        env = "FLASHBLOCKS_MAX_SEND_PEERS",
        required = false,
        default_value_t = DEFAULT_MAX_SEND_PEERS
    )]
    pub max_send_peers: usize,

    /// Override the number of receive peers maintained for flashblocks fanout.
    #[arg(
        long = "flashblocks.max_receive_peers",
        env = "FLASHBLOCKS_MAX_RECEIVE_PEERS",
        required = false,
        default_value_t = DEFAULT_MAX_RECEIVE_PEERS
    )]
    pub max_receive_peers: usize,

    /// Override the flashblocks rotation interval in seconds.
    #[arg(
        long = "flashblocks.rotation_interval",
        env = "FLASHBLOCKS_ROTATION_INTERVAL",
        required = false,
        default_value_t = DEFAULT_ROTATION_INTERVAL
    )]
    pub rotation_interval: u64,

    /// Override the number of latency samples retained for receive-peer scoring.
    #[arg(
        long = "flashblocks.score_samples",
        env = "FLASHBLOCKS_SCORE_SAMPLES",
        required = false,
        default_value_t = DEFAULT_SCORE_SAMPLES
    )]
    pub score_samples: i64,

    /// Peers to always receive flashblocks from regardless of their score.
    ///
    /// These peers will be requested as soon as they connect and will never
    /// be evicted by rotation. They count toward `max_receive_peers`.
    #[arg(
        long = "flashblocks.force_receive_peers",
        env = "FLASHBLOCKS_FORCE_RECEIVE_PEERS",
        value_delimiter = ',',
        value_name = "PEER_ID",
        required = false
    )]
    pub force_receive_peers: Vec<PeerId>,
}

impl Default for FanoutArgs {
    fn default() -> Self {
        Self {
            max_send_peers: DEFAULT_MAX_SEND_PEERS,
            max_receive_peers: DEFAULT_MAX_RECEIVE_PEERS,
            rotation_interval: DEFAULT_ROTATION_INTERVAL,
            score_samples: DEFAULT_SCORE_SAMPLES,
            force_receive_peers: Vec::new(),
        }
    }
}

/// Flashblocks configuration
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[command(
    next_help_heading = "Flashblocks",
    group = ArgGroup::new("authorizer").multiple(false)
)]
#[group(requires = "flashblocks.enabled")]
pub struct FlashblocksArgs {
    #[arg(
        long = "flashblocks.enabled",
        id = "flashblocks.enabled",
        required = false
    )]
    pub enabled: bool,

    /// Authorizer verifying key
    /// used to verify flashblock authenticity.
    #[arg(
        long = "flashblocks.authorizer_vk",
        env = "FLASHBLOCKS_AUTHORIZER_VK",
        group = "authorizer",
        value_parser = parse_vk,
        required = false,
    )]
    pub authorizer_vk: Option<VerifyingKey>,

    /// Flashblocks signing key
    /// used to sign authorized flashblocks payloads.
    #[arg(
        long = "flashblocks.builder_sk",
        env = "FLASHBLOCKS_BUILDER_SK",
        required = false,
        value_parser = parse_sk,
    )]
    pub builder_sk: Option<SigningKey>,

    /// Override incoming authorizations from rollup boost.
    #[arg(
        long = "flashblocks.override_authorizer_sk",
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
        long = "flashblocks.force_publish",
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
        requires = "builder_sk"
    )]
    pub flashblocks_interval: u64,

    /// Interval at which the block builder
    /// should re-commit to the transaction pool when building a payload.
    ///
    /// In milliseconds.
    #[arg(
        long = "flashblocks.recommit_interval",
        env = "FLASHBLOCKS_RECOMMIT_INTERVAL",
        default_value_t = 200,
        requires = "builder_sk"
    )]
    pub recommit_interval: u64,

    /// Enables flashblocks access list support.
    ///
    /// Will create access lists when building flashblocks payloads.
    /// and will use access lists for parallel transaction execution when verifying
    /// flashblocks payloads.
    #[arg(
        long = "flashblocks.access_list",
        env = "FLASHBLOCKS_ACCESS_LIST",
        default_value_t = false
    )]
    pub access_list: bool,

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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct CommandParser {
        #[command(flatten)]
        flashblocks: FlashblocksArgs,
    }

    #[test]
    fn flashblocks_override_authorizer() {
        let flashblocks = FlashblocksArgs {
            enabled: true,
            override_authorizer_sk: Some(SigningKey::from_bytes(&[0; 32])),
            authorizer_vk: None,
            builder_sk: Some(SigningKey::from_bytes(&[0; 32])),
            force_publish: false,
            recommit_interval: 200,
            flashblocks_interval: 200,
            access_list: true,
            fanout: FanoutArgs::default(),
        };

        let args = CommandParser::parse_from([
            "bin",
            "--flashblocks.enabled",
            "--flashblocks.override_authorizer_sk",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "--flashblocks.access_list",
            "--flashblocks.builder_sk",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "--flashblocks.interval",
            "200",
            "--flashblocks.recommit_interval",
            "200",
        ]);

        assert_eq!(args.flashblocks, flashblocks);
    }

    #[test]
    fn flashblocks_authorizer() {
        let flashblocks = FlashblocksArgs {
            enabled: true,
            override_authorizer_sk: None,
            authorizer_vk: Some(VerifyingKey::from_bytes(&[0; 32]).unwrap()),
            builder_sk: None,
            force_publish: false,
            recommit_interval: 200,
            flashblocks_interval: 200,
            access_list: false,
            fanout: FanoutArgs::default(),
        };

        let args = CommandParser::parse_from([
            "bin",
            "--flashblocks.enabled",
            "--flashblocks.authorizer_vk",
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
            "--flashblocks.override_authorizer",
            "--flashblocks.authorizer_vk",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ])
        .unwrap_err();
    }
}
