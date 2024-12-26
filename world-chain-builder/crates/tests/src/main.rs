//! Devnet Deployment Binary
//!     - Starts the Kurtosis Devnet
//!     - Deploys the 4337 PBH Contract Stack
//!     - Asserts all Services are running, and healthy
//! NOTE: This binary assumes that the Kurtosis Devnet is not running, and the `world-chain` enclave has been cleaned.

use std::{
    env,
    path::{Path, PathBuf},
};

use alloy_primitives::Bytes;
use eyre::eyre::{eyre, Result};
use fixtures::generate_test_fixture;
use serde::{Deserialize, Serialize};
use std::process::Command;
use tracing::info;

pub mod cases;
pub mod fixtures;

const DEPLOYER_DEV: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let (builder_rpc, sequencer_rpc) = start_devnet().await?;
    deploy_contracts(builder_rpc).await?;

    generate_test_fixture().await?;

    let sequencer_provider =
        Arc::new(ProviderBuilder::default().on_http(sequencer_socket.parse().unwrap()));
    let builder_provider =
        Arc::new(ProviderBuilder::default().on_http(builder_socket.parse().unwrap()));

    let timeout = std::time::Duration::from_secs(30);

    let f = async {
        let wait_0 = wait(sequencer_provider.clone(), timeout);
        let wait_1 = wait(builder_provider.clone(), timeout);
        tokio::join!(wait_0, wait_1);
    };
    f.await;

    info!("Devnet is ready");

    cases::assert_build(builder_provider).await?;
    cases::assert_fallback(builder_provider).await?;

    Ok(())
}

async fn start_devnet() -> Result<(String, String)> {
    run_command(
        &"kurtosis run",
        &[
            ".",
            "--args-file",
            "network_params.yaml",
            "--enclave",
            "world-chain",
        ],
        "../../../../devnet/",
    )
    .await?;
    print!("Devnet is running");
    let builder_socket = run_command(
        "kurtosis",
        &[
            "port",
            "print",
            "world-chain",
            "wc-admin-world-chain-builder",
            "rpc",
        ],
        "../../../../devnet/",
    )
    .await?;

    let builder_socket = format!(
        "http://{}",
        builder_socket.split("http://").collect::<Vec<&str>>()[1]
    );

    let sequencer_socket = run_command(
        "kurtosis",
        &["port", "print", "world-chain", "wc-admin-op-geth", "rpc"],
        "../../../../devnet/",
    )
    .await?;

    let sequencer_socket = format!(
        "http://{}",
        sequencer_socket.split("http://").collect::<Vec<&str>>()[1]
    );

    info!(
        builder_rpc = %builder_socket,
        sequencer_rpc = %sequencer_socket,
        "Devnet is ready"
    );

    Ok((builder_socket, sequencer_socket))
}

async fn deploy_contracts(builder_rpc: String) -> Result<()> {
    if std::env::var("PRIVATE_KEY").is_err() {
        std::env::set_var("PRIVATE_KEY", DEPLOYER_DEV);
    }
    run_command(
        "forge",
        &[
            "script",
            "scripts/DeployDevnet.s.sol:DeployDevnet",
            "--rpc-url",
            &builder_rpc,
            "--broadcast",
        ],
        "../../../../devnet/",
    )
    .await?;
    Ok(())
}

async fn wait<T, P>(provider: P, timeout: time::Duration)
where
    T: Transport + Clone,
    P: Provider<T>,
{
    let start = Instant::now();
    loop {
        if provider
            .get_block_by_number(BlockNumberOrTag::Latest, BlockTransactionsKind::Hashes)
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

pub async fn run_command(cmd: &str, args: &[&str], ctx: impl AsRef<Path>) -> Result<String> {
    let output = Command::new(cmd).args(args).current_dir(ctx).output()?;
    if output.status.success() {
        Ok(String::from_utf8(output.stdout)?)
    } else {
        Err(eyre!(
            "Command failed: {:?}",
            String::from_utf8(output.stdout).unwrap(),
        ))
    }
}
