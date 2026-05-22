use std::{path::PathBuf, time::Duration};

use eyre::Result;
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{IntoContainerPort, Mount, WaitFor},
    runners::AsyncRunner,
};
use tracing::info;

use crate::process_logs::{ProcessLogTarget, container_log_consumer};

const ANVIL_RPC_PORT: u16 = 8545;

/// Configuration for the containerized L1 dev chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct L1DevChainConfig {
    /// Docker image name.
    pub image: String,
    /// Docker image tag.
    pub tag: String,
    /// L1 chain ID passed to anvil.
    pub chain_id: u64,
    /// L1 block time in seconds.
    pub block_time_secs: u64,
    /// Optional stable host port.
    pub stable_port: Option<u16>,
    /// Optional geth-style genesis file to initialize anvil with.
    pub genesis_file: Option<PathBuf>,
}

impl Default for L1DevChainConfig {
    fn default() -> Self {
        Self {
            image: "ghcr.io/foundry-rs/foundry".to_string(),
            tag: "latest".to_string(),
            chain_id: 31_337,
            block_time_secs: 2,
            stable_port: None,
            genesis_file: None,
        }
    }
}

/// Lifecycle-owned L1 dev chain container.
#[derive(Debug)]
pub struct L1DevChain {
    rpc_url: String,
    _container: ContainerAsync<GenericImage>,
}

impl L1DevChain {
    /// Start an anvil-backed L1 dev chain through testcontainers.
    pub async fn start(config: L1DevChainConfig) -> Result<Self> {
        info!(
            image = %format!("{}:{}", config.image, config.tag),
            chain_id = config.chain_id,
            block_time_secs = config.block_time_secs,
            stable_port = ?config.stable_port,
            genesis_file = ?config.genesis_file,
            "starting L1 dev chain container"
        );

        let image = GenericImage::new(config.image, config.tag)
            .with_entrypoint("anvil")
            .with_exposed_port(ANVIL_RPC_PORT.tcp())
            .with_wait_for(WaitFor::message_on_stdout("Listening on"))
            .with_log_consumer(container_log_consumer(
                "l1-dev-chain",
                ProcessLogTarget::L1DevChain,
            ));

        let mut cmd = vec![
            "--host".to_string(),
            "0.0.0.0".to_string(),
            "--port".to_string(),
            ANVIL_RPC_PORT.to_string(),
            "--chain-id".to_string(),
            config.chain_id.to_string(),
            "--block-time".to_string(),
            config.block_time_secs.to_string(),
        ];

        if let Some(genesis_file) = &config.genesis_file {
            let file_name = genesis_file
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("l1-genesis.json");
            cmd.push("--init".to_string());
            cmd.push(format!("/work/{file_name}"));
        }

        let mut request = image
            .with_cmd(cmd)
            .with_startup_timeout(Duration::from_secs(90));

        if let Some(genesis_file) = &config.genesis_file {
            let parent = genesis_file
                .parent()
                .unwrap_or(genesis_file.as_path());
            request = request.with_mount(Mount::bind_mount(
                parent.to_string_lossy().to_string(),
                "/work",
            ));
        }

        if let Some(host_port) = config.stable_port {
            request = request.with_mapped_port(host_port, ANVIL_RPC_PORT.tcp());
        }

        let container = request.start().await?;
        let host = container.get_host().await?;
        let port = container.get_host_port_ipv4(ANVIL_RPC_PORT.tcp()).await?;
        let rpc_url = format!("http://{host}:{port}");

        info!(%rpc_url, "L1 dev chain ready");

        Ok(Self {
            rpc_url,
            _container: container,
        })
    }

    /// HTTP RPC URL for the L1 dev chain.
    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }
}
