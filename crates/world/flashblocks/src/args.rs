//! Additional Node command arguments.

//! clap [Args](clap::Args) for Flashblocks configuration

use std::net::{IpAddr, Ipv4Addr};

/// Parameters for Flashblocks configuration
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "Flashblocks")]
pub struct FlashblockArgs {
    /// The payload building interval in milliseconds.
    #[arg(long = "flashblock.block_time", env, default_value = "1000")]
    pub flashblock_block_time: u64,

    /// Interval in milliseconds to wait before computing the next pending block.
    #[arg(long = "flashblock.interval", env, default_value = "250")]
    pub flashblock_interval: u64,

    /// The host to bind the Flashblocks WebSocket server to.
    #[arg(long = "flashblock.host", env, default_value = "127.0.0.1")]
    pub flashblock_host: IpAddr,

    /// The port to bind the Flashblocks WebSocket server to.
    #[arg(long = "flashblock.port", env, default_value = "9002")]
    pub flashblock_port: u16,
}

impl Default for FlashblockArgs {
    fn default() -> Self {
        Self {
            flashblock_block_time: 1000,
            flashblock_interval: 250,
            flashblock_host: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            flashblock_port: 9002,
        }
    }
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
        let expected_args = FlashblockArgs {
            flashblock_block_time: 1,
            flashblock_interval: 200,
            flashblock_port: 9002,
            flashblock_host: "127.0.0.1".parse().unwrap(),
        };

        let args = CommandParser::<FlashblockArgs>::parse_from([
            "bin",
            "--flashblock.block_time",
            "1",
            "--flashblock.interval",
            "200",
        ])
        .args;

        assert_eq!(args, expected_args);
    }
}
