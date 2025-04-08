//! Additional Node command arguments.

//! clap [Args](clap::Args) for Flashblocks configuration

/// Parameters for Flashblocks configuration
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "Flashblocks")]
pub struct FlashblockArgs {
    /// The Chain's block time
    #[arg(long = "flashblock.block_time")]
    pub block_time: u64,

    /// Interval in milliseconds to wait before computing the next pending block.
    #[arg(long = "flashblock.interval")]
    pub interval: u64,
}

impl Default for FlashblockArgs {
    fn default() -> Self {
        Self {
            block_time: 2,
            interval: 250,
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
    fn test_parse_flashblocks_block_time_arg() {
        let expected_args = FlashblockArgs {
            block_time: 1,
            ..Default::default()
        };
        let args =
            CommandParser::<FlashblockArgs>::parse_from(["--flashblock.block_time", "1"]).args;
        assert_eq!(args, expected_args);
    }

    #[test]
    fn test_parse_flashblocks_interval() {
        let expected_args = FlashblockArgs {
            interval: 200,
            ..Default::default()
        };
        let args =
            CommandParser::<FlashblockArgs>::parse_from(["--flashblock.interval", "200"]).args;
        assert_eq!(args, expected_args);
    }
}
