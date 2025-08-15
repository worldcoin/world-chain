//! Additional Node command arguments.

//! clap [Args](clap::Args) for Flashblocks configuration

use std::net::IpAddr;

use rollup_boost::{
    ed25519_dalek::{SigningKey, VerifyingKey},
    parse_sk, parse_vk,
};

/// Parameters for Flashblocks configuration
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "Flashblocks")]
pub struct FlashblocksArgs {
    /// The payload building interval in milliseconds.
    #[arg(long = "flashblock.block_time", env, default_value = "1000")]
    pub flashblock_block_time: u64,

    /// Interval in milliseconds to wait before computing the next pending block.
    #[arg(long = "flashblock.interval", env, default_value = "200")]
    pub flashblock_interval: u64,

    /// The host to bind the Flashblocks WebSocket server to.
    #[arg(long = "flashblock.host", env, default_value = "127.0.0.1")]
    pub flashblock_host: IpAddr,

    /// The port to bind the Flashblocks WebSocket server to.
    #[arg(long = "flashblock.port", env, default_value = "9002")]
    pub flashblock_port: u16,

    #[arg(
        long = "flashblock.authorizor_sk",
        env = "FLASHBLOCKS_AUTHORIZOR_VK", 
        value_parser = parse_vk,
        required = false,
    )]
    pub flashblocks_authorizor_vk: Option<VerifyingKey>,

    #[arg(long = "flashblock.builder_sk", 
        env = "FLASHBLOCKS_BUILDER_SK", 
        value_parser = parse_sk,
        required = false,
    )]
    pub flashblocks_builder_sk: SigningKey,

    #[arg(
        long = "flashblock.authorization_enabled",
        env,
        default_value_t = false,
        help = "Enable authorization for flashblocks"
    )]
    pub flashblocks_authorization_enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Args, Parser};

    /// A helper type to parse Args more easily
    #[derive(Parser)]
    struct CommandParser<T: Args> {
        #[command(flatten)]
        args: T,
    }

    #[test]
    fn parse_args() {
        let expected_args = FlashblocksArgs {
            flashblock_block_time: 1,
            flashblock_interval: 200,
            flashblock_port: 9002,
            flashblock_host: "127.0.0.1".parse().unwrap(),
            flashblocks_authorizor_vk: None,
            flashblocks_builder_sk: SigningKey::from_bytes(&[0; 32]),
            flashblocks_authorization_enabled: false,
        };

        let args = CommandParser::<FlashblocksArgs>::parse_from([
            "bin",
            "--flashblock.block_time",
            "1",
            "--flashblock.interval",
            "200",
            "--flashblock.builder_sk",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ])
        .args;

        assert_eq!(args, expected_args);
    }
}
