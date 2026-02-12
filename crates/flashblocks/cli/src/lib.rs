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
        long = "flashblocks.spoof_authorizer_sk",
        env = "FLASHBLOCKS_SPOOF_AUTHORIZER_SK",
        group = "authorizer",
        requires = "builder_sk",
        conflicts_with = "authorizer_vk",
        required = false,
        value_parser = parse_sk,
    )]
    pub spoof_authorizer_sk: Option<SigningKey>,

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
}

pub fn parse_sk(s: &str) -> eyre::Result<SigningKey> {
    let bytes = <[u8; 32]>::from_hex(s.trim())?;
    Ok(SigningKey::from_bytes(&bytes))
}

pub fn parse_vk(s: &str) -> eyre::Result<VerifyingKey> {
    let bytes = <[u8; 32]>::from_hex(s.trim())?;
    Ok(VerifyingKey::from_bytes(&bytes)?)
}

pub use flashblocks_builder::FlashblocksPayloadBuilderConfig;

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
            spoof_authorizer_sk: Some(SigningKey::from_bytes(&[0; 32])),
            authorizer_vk: None,
            builder_sk: Some(SigningKey::from_bytes(&[0; 32])),
            recommit_interval: 200,
            flashblocks_interval: 200,
            access_list: true,
        };

        let args = CommandParser::parse_from([
            "bin",
            "--flashblocks.enabled",
            "--flashblocks.spoof_authorizer_sk",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "--flashblocks.access_list",
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
            spoof_authorizer_sk: None,
            authorizer_vk: Some(VerifyingKey::from_bytes(&[0; 32]).unwrap()),
            builder_sk: None,
            recommit_interval: 200,
            flashblocks_interval: 200,
            access_list: false,
        };

        let args = CommandParser::parse_from([
            "bin",
            "--flashblocks.enabled",
            "--flashblocks.authorizer_vk",
            "0000000000000000000000000000000000000000000000000000000000000000",
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
