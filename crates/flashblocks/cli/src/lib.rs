use clap::ArgGroup;
use ed25519_dalek::{SigningKey, VerifyingKey};
use hex::FromHex;

/// Flashblocks configuration
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "Flashblocks",
    group = ArgGroup::new("authorizer")
        .multiple(false)
)]
#[group(requires = "flashblocks.enabled")]
pub struct FlashblocksArgs {
    #[arg(
        long = "flashblocks.enabled",
        id = "flashblocks.enabled",
        requires = "authorizer",
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

    /// Uses the builder_sk to create spoofed
    /// flashblocks authorizations.
    ///
    /// Should only be used for testing
    #[arg(
        long = "flashblocks.spoof_authorizer",
        env = "FLASHBLOCKS_SPOOF_AUTHORIZER",
        group = "authorizer",
        requires = "builder_sk",
        required = false
    )]
    pub spoof_authorizer: bool,

    /// The interval to publish pre-confirmations
    /// when building a payload in milliseconds.
    #[arg(
        long = "flashblocks.interval",
        env = "FLASHBLOCKS_INTERVAL",
        default_value_t = 200
    )]
    pub interval: u64,

    /// Interval at which the block builder
    /// should re-commit to the transaction pool when building a payload.
    ///
    /// In milliseconds.
    #[arg(
        long = "flashblocks.recommit_interval",
        env = "FLASHBLOCKS_RECOMMIT_INTERVAL",
        default_value_t = 200
    )]
    pub recommit_interval: u64,

    /// The maximum number of concurrent payload build tasks.
    #[arg(
        long = "flashblocks.max_payload_tasks",
        env = "FLASHBLOCKS_MAX_PAYLOAD_TASKS",
        default_value_t = 50
    )]
    pub max_payload_tasks: usize,
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
        flashblocks: Option<FlashblocksArgs>,
    }

    #[test]
    fn flashblocks_spoof_authorizer() {
        let flashblocks = FlashblocksArgs {
            enabled: true,
            spoof_authorizer: true,
            authorizer_vk: None,
            builder_sk: Some(SigningKey::from_bytes(&[0; 32])),
            recommit_interval: 200,
            interval: 200,
            max_payload_tasks: 50,
        };

        let args = CommandParser::parse_from([
            "bin",
            "--flashblocks.enabled",
            "--flashblocks.spoof_authorizer",
            "--flashblocks.builder_sk",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "--flashblocks.interval",
            "200",
            "--flashblocks.recommit_interval",
            "200",
        ]);

        assert_eq!(args.flashblocks.unwrap(), flashblocks);
    }

    #[test]
    fn flashblocks_authorizer() {
        let flashblocks = FlashblocksArgs {
            enabled: true,
            spoof_authorizer: false,
            authorizer_vk: Some(VerifyingKey::from_bytes(&[0; 32]).unwrap()),
            builder_sk: None,
            recommit_interval: 200,
            interval: 200,
            max_payload_tasks: 50,
        };

        let args = CommandParser::parse_from([
            "bin",
            "--flashblocks.enabled",
            "--flashblocks.authorizer_vk",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "--flashblocks.interval",
            "200",
            "--flashblocks.recommit_interval",
            "200",
        ]);

        assert_eq!(args.flashblocks.unwrap(), flashblocks);
    }

    #[test]
    fn flashblocks_neither() {
        CommandParser::try_parse_from(["bin", "--flashblocks.enabled"]).unwrap_err();
    }

    #[test]
    fn flashblocks_both() {
        CommandParser::try_parse_from([
            "bin",
            "--flashblocks.enabled",
            "--flashblocks.spoof_authorizer",
            "--flashblocks.authorizer_vk",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ])
        .unwrap_err();
    }
}
