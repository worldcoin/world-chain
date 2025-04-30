//! Additional Node command arguments.

//! clap [Args](clap::Args) for Flashblocks configuration

/// Parameters for Flashblocks configuration
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "Flashblocks")]
pub struct FlashblockArgs {
    /// The Chain's block time
    #[arg(long = "flashblock.block_time")]
    pub flashblock_block_time: u64,

    /// Interval in milliseconds to wait before computing the next pending block.
    #[arg(long = "flashblock.interval")]
    pub flashblock_interval: u64,
}

impl Default for FlashblockArgs {
    fn default() -> Self {
        Self {
            flashblock_block_time: 2,
            flashblock_interval: 250,
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
