//! `sp1-worker` binary: leases SP1 proof jobs from the `prover-service`, proves them, and
//! submits the proofs back.

mod cmd;

use anyhow::Result;
use clap::{Parser, Subcommand};
use cmd::{deposit::DepositArgs, run::WorkerArgs, vkeys::VkeysArgs};

#[derive(Debug, Parser)]
#[command(
    name = "sp1-worker",
    about = "World Chain SP1 proving worker and funding utility"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the SP1 proving worker.
    Run(Box<WorkerArgs>),
    /// Deposit PROVE into the Succinct VApp for this worker's SP1 Network account.
    Deposit(DepositArgs),
    /// Print or verify the vkeys of the SP1 guest ELFs embedded in this worker.
    Vkeys(VkeysArgs),
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    match Cli::parse().command {
        Command::Run(args) => {
            let _telemetry_guard = telemetry_batteries::init()
                .map_err(|error| anyhow::anyhow!("failed to initialize telemetry: {error:#}"))?;
            world_chain_proof_metrics::describe_metrics();
            cmd::run::run(*args).await
        }
        Command::Deposit(args) => cmd::deposit::deposit(args).await,
        Command::Vkeys(args) => cmd::vkeys::vkeys(args).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_run_subcommand() {
        let cli = Cli::parse_from([
            "sp1-worker",
            "run",
            "--prover-service-url",
            "http://127.0.0.1:8545",
            "--l2-rpc",
            "http://127.0.0.1:9545",
            "--l1-rpc",
            "http://127.0.0.1:8545",
            "--l1-beacon-rpc",
            "http://127.0.0.1:5052",
            "--worker-id",
            "test",
            "--sp1-kms-key-id",
            "alias/prover",
        ]);

        assert!(matches!(cli.command, Command::Run(_)));
    }

    #[test]
    fn parses_deposit_subcommand() {
        let cli = Cli::parse_from([
            "sp1-worker",
            "deposit",
            "--amount",
            "100",
            "--sp1-network-l1-rpc-url",
            "https://ethereum.example",
            "--succinct-vapp-address",
            "0x0000000000000000000000000000000000000001",
            "--sp1-private-key",
            "test-key",
        ]);

        assert!(matches!(cli.command, Command::Deposit(_)));
    }

    #[test]
    fn deposit_requires_exactly_one_signer() {
        let base_args = || {
            vec![
                "sp1-worker",
                "deposit",
                "--amount",
                "100",
                "--sp1-network-l1-rpc-url",
                "https://ethereum.example",
                "--succinct-vapp-address",
                "0x0000000000000000000000000000000000000001",
            ]
        };

        assert_eq!(
            Cli::try_parse_from(base_args()).unwrap_err().kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );

        let mut multiple = base_args();
        multiple.extend([
            "--sp1-private-key",
            "0x1234",
            "--sp1-kms-key-id",
            "alias/prover",
        ]);
        assert_eq!(
            Cli::try_parse_from(multiple).unwrap_err().kind(),
            clap::error::ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn parses_vkeys_subcommand() {
        let cli = Cli::parse_from(["sp1-worker", "vkeys", "--check", "vkeys.json"]);
        assert!(matches!(cli.command, Command::Vkeys(_)));
    }
}
