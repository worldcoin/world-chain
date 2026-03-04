use clap::ArgGroup;
use ed25519_dalek::{SigningKey, VerifyingKey};
use hex::FromHex;
use reth_network_peers::TrustedPeer;

pub const DEFAULT_FLASHBLOCKS_BOOTNODES: &str = "enode://78ca7daeb63956cbc3985853d5699a6404d976a2612575563f46876968fdca2383a195ee7db40de348757b2256195996933708f351169ca3f3fe93ab2a774608@16.62.98.53:30303,enode://c96dcadf4cdea4c39ec3fd775637d9e67d455b856b1514cfcf55b72f873a34b96d69e47ccea9fc797a446d4e6948aa80f6b9d479a1727ca166758a900b08f422@16.63.14.166:30303,enode://15688a7b281c32a4da633252dcc5019d60f037ee9eb46d05093dd3023bdd688b9b207d10a39e054a5ed87db666b2cb75696f6537de74d1e1f8dcabc53dc8d2ab@16.63.123.160:30303";

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

    /// Comma-separated list of flashblocks bootnode enodes.
    ///
    /// These peers are set as trusted peers in the network.
    #[arg(
        long = "flashblocks.bootnodes",
        env = "FLASHBLOCKS_BOOTNODES",
        value_delimiter = ',',
        value_name = "ENODE",
        default_value = DEFAULT_FLASHBLOCKS_BOOTNODES
    )]
    pub bootnodes: Vec<TrustedPeer>,
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

    fn default_bootnodes() -> Vec<TrustedPeer> {
        DEFAULT_FLASHBLOCKS_BOOTNODES
            .split(',')
            .map(|enode| enode.parse().unwrap())
            .collect()
    }

    #[derive(Debug, Parser)]
    struct CommandParser {
        #[command(flatten)]
        flashblocks: Option<FlashblocksArgs>,
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
            bootnodes: default_bootnodes(),
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

        assert_eq!(args.flashblocks.unwrap(), flashblocks);
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
            bootnodes: default_bootnodes(),
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

    #[test]
    fn flashblocks_bootnodes() {
        let bootnodes = vec![
            "enode://6f8a80d14311c39f35f516fa664deaaaa13e85b2f7493f37f6144d86991ec012937307647bd3b9a82abe2974e1407241d54947bbb39763a4cac9f77166ad92a0@10.3.58.6:30303"
                .parse()
                .unwrap(),
            "enode://d860a01f9722d78051619d1e2351aba3f43f943f6f00718d1b9baa4101932a1f5011f16bb2b1bb35db20d6db18b2a4b46dcd226f73d917f6652a2b0a96b4f78a@10.3.58.7:30303"
                .parse()
                .unwrap(),
        ];
        let flashblocks = FlashblocksArgs {
            enabled: true,
            override_authorizer_sk: None,
            authorizer_vk: None,
            builder_sk: None,
            force_publish: false,
            recommit_interval: 200,
            flashblocks_interval: 200,
            access_list: false,
            bootnodes,
        };

        let args = CommandParser::parse_from([
            "bin",
            "--flashblocks.enabled",
            "--flashblocks.bootnodes",
            "enode://6f8a80d14311c39f35f516fa664deaaaa13e85b2f7493f37f6144d86991ec012937307647bd3b9a82abe2974e1407241d54947bbb39763a4cac9f77166ad92a0@10.3.58.6:30303,enode://d860a01f9722d78051619d1e2351aba3f43f943f6f00718d1b9baa4101932a1f5011f16bb2b1bb35db20d6db18b2a4b46dcd226f73d917f6652a2b0a96b4f78a@10.3.58.7:30303",
        ]);

        assert_eq!(args.flashblocks.unwrap(), flashblocks);
    }
}
