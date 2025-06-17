//! Devnet Deployment Binary
//!     - Starts the Kurtosis Devnet
//!     - Deploys the 4337 PBH Contract Stack
//!     - Asserts all Services are running, and healthy
//! NOTE: This binary assumes that the Kurtosis Devnet is not running, and the `world-chain` enclave has been cleaned.

use std::{
    env,
    path::Path,
    process::Stdio,
    sync::Arc,
    time::{self, Duration, Instant},
};

use alloy_network::Network;
use alloy_provider::network::Ethereum;
use alloy_provider::RootProvider;
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types_eth::BlockNumberOrTag;
use clap::Parser;
use eyre::eyre;
use eyre::Result;
use fixtures::TransactionFixtures;
use std::process::Command;
use tokio::time::sleep;
use tracing::{error, info};
pub mod cases;
pub mod fixtures;

#[derive(Parser)]
pub struct Args {
    #[clap(short, long, default_value_t = false)]
    pub no_deploy: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let (builder_rpc, sequencer_rpc, rundler, tx_proxy) = start_devnet(args).await?;

    let sequencer_provider: Arc<RootProvider<Ethereum>> =
        Arc::new(ProviderBuilder::default().connect_http(sequencer_rpc.parse().unwrap()));
    let builder_provider: Arc<RootProvider<Ethereum>> =
        Arc::new(ProviderBuilder::default().connect_http(builder_rpc.parse().unwrap()));
    let rundler_provider: Arc<RootProvider<Ethereum>> =
        Arc::new(ProviderBuilder::default().connect_http(rundler.parse().unwrap()));
    let tx_proxy_provider: Arc<RootProvider<Ethereum>> =
        Arc::new(ProviderBuilder::default().connect_http(tx_proxy.parse().unwrap()));

    let timeout = std::time::Duration::from_secs(30);

    info!("Waiting for the devnet to be ready");

    let f = async {
        let wait_0 = wait(sequencer_provider.clone(), timeout);
        let wait_1 = wait(builder_provider.clone(), timeout);
        tokio::join!(wait_0, wait_1);
    };
    f.await;

    let fixture = TransactionFixtures::new().await;
    info!("Running Rundler Ops test");
    cases::user_ops_test(
        rundler_provider,
        builder_provider.clone(),
        fixture.pbh_user_operations,
    )
    .await?;
    info!("Running load test");
    cases::load_test(
        tx_proxy_provider,
        builder_provider.clone(),
        &fixture.eip1559[..198],
    )
    .await?;
    info!("Running Transact Conditional Test");
    cases::transact_conditional_test(builder_provider.clone(), &fixture.eip1559[198..]).await?;
    info!("Running fallback test");
    cases::fallback_test(sequencer_provider.clone()).await?;
    Ok(())
}

async fn start_devnet(args: Args) -> Result<(String, String, String, String)> {
    if !args.no_deploy {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .canonicalize()?;

        let mut command = Command::new("just")
            .current_dir(path)
            .args(["devnet-up"])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;

        let status = command.wait()?;
        if !status.success() {
            eyre::bail!("Failed to start the devnet");
        }
    }

    let (builder_socket, sequencer_socket, rundler, tx_proxy) = get_endpoints().await?;

    info!(
        builder_rpc = %builder_socket,
        sequencer_rpc = %sequencer_socket,
        rundler_rpc = %rundler,
        "Devnet is ready"
    );

    Ok((builder_socket, sequencer_socket, rundler, tx_proxy))
}

async fn get_endpoints() -> Result<(String, String, String, String)> {
    let builder_socket = run_command(
        "kurtosis",
        &[
            "port",
            "print",
            "world-chain",
            "op-el-builder-2151908-1-custom-op-node-op-kurtosis",
            "rpc",
        ],
        env!("CARGO_MANIFEST_DIR"),
    )
    .await?;

    let builder_socket = format!(
        "http://{}",
        builder_socket.split("http://").collect::<Vec<&str>>()[1]
    );

    let sequencer_socket = run_command(
        "kurtosis",
        &[
            "port",
            "print",
            "world-chain",
            "op-el-2151908-1-op-geth-op-node-op-kurtosis",
            "rpc",
        ],
        env!("CARGO_MANIFEST_DIR"),
    )
    .await?;

    let rundler_socket = run_command(
        "kurtosis",
        &["port", "print", "world-chain", "rundler", "rpc"],
        env!("CARGO_MANIFEST_DIR"),
    )
    .await?;

    let sequencer_socket = format!(
        "http://{}",
        sequencer_socket.split("http://").collect::<Vec<&str>>()[1]
    );

    let tx_proxy_socket = run_command(
        "kurtosis",
        &["port", "print", "world-chain", "tx-proxy", "rpc"],
        env!("CARGO_MANIFEST_DIR"),
    )
    .await?;

    Ok((
        builder_socket,
        sequencer_socket,
        rundler_socket,
        tx_proxy_socket,
    ))
}

async fn wait<N, P>(provider: P, timeout: time::Duration)
where
    N: Network,
    P: Provider<N>,
{
    let start = Instant::now();
    loop {
        if provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await
            .is_ok()
        {
            break;
        }
        sleep(Duration::from_secs(1)).await;
        if start.elapsed() > timeout {
            panic!("Timeout waiting for the devnet");
        }
    }
}

pub async fn run_command(cmd: &str, args: &[&str], cwd: impl AsRef<Path>) -> Result<String> {
    let cwd = cwd.as_ref();
    let output = Command::new(cmd).current_dir(cwd).args(args).output()?;
    if output.status.success() {
        let stdout = String::from_utf8(output.stdout)?;
        info!("{:?}", stdout.trim_end_matches(r#"\n"#));
        Ok(stdout)
    } else {
        let aggregate_cmd = std::iter::once(cmd)
            .chain(args.iter().copied())
            .collect::<Vec<&str>>()
            .join(" ");
        let stdout = String::from_utf8(output.stdout)?;
        let stderr = String::from_utf8(output.stderr)?;

        error!(
            "Command {aggregate_cmd} in {cwd} failed:",
            cwd = cwd.display()
        );
        error!("{stdout}");
        error!("{stderr}");
        eyre::bail!("Command {aggregate_cmd} failed: {stdout}\n{stderr}",)
    }
}
