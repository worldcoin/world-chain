use std::{path::PathBuf, process::Stdio, time::Duration};

use eyre::eyre::{Context, Result, eyre};
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{IntoContainerPort, Mount, WaitFor},
    runners::AsyncRunner,
};
use tokio::{fs, io::AsyncWriteExt};
use tracing::info;

use crate::process_logs::{ProcessLogTarget, container_log_consumer};

const ANVIL_RPC_PORT: u16 = 8545;

/// Configuration for the simple anvil-backed L1 dev chain. Used by the DirectSequencer / Minimal
/// presets, which do not run the proof system and therefore do not need a real EL debug namespace
/// or a beacon. The HaSequencer / full-stack proof-system devnet uses [`L1Stack`] instead.
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

/// Lifecycle-owned anvil L1 dev chain container.
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
            let parent = genesis_file.parent().unwrap_or(genesis_file.as_path());
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

// ---------------------------------------------------------------------------
// Post-merge L1 stack (reth EL + Lighthouse CL) for the proof-system devnet.
//
// anvil does not implement `debug_getRawHeader` / `debug_getRawReceipts`, which kona's witness
// host requires to fetch L1 headers and receipts during derivation; reth does, and Lighthouse
// provides a real beacon API (`/eth/v1/beacon/genesis`, `/eth/v1/config/spec`, blob sidecars).
// Mirrors Base's devnet L1. Containers reach reth/each other via `host.docker.internal:<port>`,
// matching the rest of the devnet (no dedicated Docker network).
// ---------------------------------------------------------------------------

/// reth L1 execution-layer HTTP RPC port (in-container).
const RETH_RPC_PORT: u16 = 8545;
/// reth L1 Engine API (authrpc) port (in-container).
const RETH_ENGINE_PORT: u16 = 8551;
/// Lighthouse beacon node HTTP API port (in-container).
const LIGHTHOUSE_BEACON_PORT: u16 = 5052;
/// Container-reachable host alias.
const DOCKER_HOST: &str = "host.docker.internal";

/// Configuration for the post-merge L1 stack (reth EL + Lighthouse CL).
#[derive(Clone, Debug)]
pub struct L1StackConfig {
    /// reth EL image and tag.
    pub reth_image: String,
    pub reth_tag: String,
    /// Lighthouse (beacon node + validator client) image and tag.
    pub lighthouse_image: String,
    pub lighthouse_tag: String,
    /// ethpandaops genesis generator image and tag (builds the CL genesis from the EL genesis).
    pub genesis_generator_image: String,
    pub genesis_generator_tag: String,
    /// L1 chain id (must match the EL genesis `config.chainId`).
    pub chain_id: u64,
    /// L1 slot duration in seconds (`SECONDS_PER_SLOT`).
    pub slot_duration_secs: u64,
    /// Unix genesis timestamp shared by the EL genesis and the CL config (must be ~now).
    pub genesis_timestamp: u64,
    /// Post-merge EL geth genesis (OP L1 contracts in `alloc`), from op-deployer's `l1StateDump`.
    pub el_genesis_path: PathBuf,
    /// Shared Engine API JWT secret file (hex).
    pub jwt_path: PathBuf,
    /// Working directory under which the generated CL testnet dir is written.
    pub workdir: PathBuf,
    /// Optional stable host port for the reth RPC.
    pub stable_rpc_port: Option<u16>,
    /// Optional stable host port for the Lighthouse beacon API.
    pub stable_beacon_port: Option<u16>,
}

/// Lifecycle-owned post-merge L1 stack: reth EL + Lighthouse beacon node + validator client.
#[derive(Debug)]
pub struct L1Stack {
    rpc_url: String,
    beacon_url: String,
    _reth: ContainerAsync<GenericImage>,
    _beacon: ContainerAsync<GenericImage>,
    _validator: ContainerAsync<GenericImage>,
}

impl L1Stack {
    /// Generates the CL genesis from the EL genesis, then brings up reth, the Lighthouse beacon
    /// node, and the Lighthouse validator client.
    pub async fn start(config: L1StackConfig) -> Result<Self> {
        info!(
            reth = %format!("{}:{}", config.reth_image, config.reth_tag),
            lighthouse = %format!("{}:{}", config.lighthouse_image, config.lighthouse_tag),
            chain_id = config.chain_id,
            slot_duration_secs = config.slot_duration_secs,
            "starting post-merge L1 (reth + Lighthouse)"
        );

        let data_dir = generate_cl_genesis(&config)
            .await
            .wrap_err("failed to generate L1 consensus-layer genesis")?;

        let (reth, rpc_url, engine_host_port) = start_reth(&config).await?;
        let (beacon, beacon_url) = start_beacon(&config, &data_dir, engine_host_port).await?;
        let validator = start_validator(&config, &data_dir, beacon_host_port(&beacon_url)?).await?;

        info!(%rpc_url, %beacon_url, "post-merge L1 ready");

        Ok(Self {
            rpc_url,
            beacon_url,
            _reth: reth,
            _beacon: beacon,
            _validator: validator,
        })
    }

    /// HTTP RPC URL for the L1 execution layer (reth). Serves the `debug` namespace.
    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    /// HTTP URL for the L1 beacon API (Lighthouse).
    pub fn beacon_url(&self) -> &str {
        &self.beacon_url
    }
}

/// Runs the ethpandaops genesis generator in `cl` mode to build the beacon-chain genesis
/// (`genesis.ssz`, `config.yaml`, validator keys) from the already-produced EL genesis, and
/// returns the host path of the resulting testnet dir.
async fn generate_cl_genesis(config: &L1StackConfig) -> Result<PathBuf> {
    let data_dir = config.workdir.join("l1-cl");
    let metadata_dir = data_dir.join("metadata");
    fs::create_dir_all(&metadata_dir)
        .await
        .wrap_err("failed to create L1 CL genesis dirs")?;

    // The generator's `cl` mode reads the EL genesis from `/data/metadata/genesis.json`.
    fs::copy(&config.el_genesis_path, metadata_dir.join("genesis.json"))
        .await
        .wrap_err("failed to stage EL genesis for CL generation")?;

    // Minimal single-validator post-merge testnet; all forks active at genesis, no delay.
    let values_env = format!(
        "export CHAIN_ID=\"{chain_id}\"\n\
         export DEPOSIT_CONTRACT_ADDRESS=\"0x0000000000000000000000000000000000000000\"\n\
         export GENESIS_TIMESTAMP=\"{genesis_ts}\"\n\
         export GENESIS_DELAY=\"0\"\n\
         export SECONDS_PER_SLOT=\"{slot}\"\n\
         export PRESET_BASE=\"minimal\"\n\
         export NUMBER_OF_VALIDATORS=\"1\"\n\
         export MIN_GENESIS_ACTIVE_VALIDATOR_COUNT=\"1\"\n\
         export TERMINAL_TOTAL_DIFFICULTY=\"0\"\n\
         export ALTAIR_FORK_EPOCH=\"0\"\n\
         export BELLATRIX_FORK_EPOCH=\"0\"\n\
         export CAPELLA_FORK_EPOCH=\"0\"\n\
         export DENEB_FORK_EPOCH=\"0\"\n\
         export ELECTRA_FORK_EPOCH=\"0\"\n\
         export FULU_FORK_EPOCH=\"18446744073709551615\"\n\
         export EL_AND_CL_MNEMONIC=\"{mnemonic}\"\n",
        chain_id = config.chain_id,
        genesis_ts = config.genesis_timestamp,
        slot = config.slot_duration_secs,
        mnemonic = DEVNET_MNEMONIC,
    );
    let values_path = data_dir.join("values.env");
    fs::write(&values_path, values_env)
        .await
        .wrap_err("failed to write CL genesis values.env")?;

    let image = format!(
        "{}:{}",
        config.genesis_generator_image, config.genesis_generator_tag
    );
    let generator = GenericImage::new(
        config.genesis_generator_image.clone(),
        config.genesis_generator_tag.clone(),
    )
    .with_wait_for(WaitFor::seconds(1))
    .with_log_consumer(container_log_consumer(
        "l1-genesis-gen",
        ProcessLogTarget::L1DevChain,
    ))
    .with_cmd(vec!["cl".to_string()])
    .with_mount(Mount::bind_mount(
        data_dir.to_string_lossy().to_string(),
        "/data",
    ))
    .with_mount(Mount::bind_mount(
        values_path.to_string_lossy().to_string(),
        "/config/values.env",
    ))
    .with_startup_timeout(Duration::from_secs(180));

    info!(image = %image, "generating L1 CL genesis");
    let container = generator
        .start()
        .await
        .wrap_err("CL genesis generator failed to start")?;
    wait_for_file(&metadata_dir.join("genesis.ssz"), Duration::from_secs(180))
        .await
        .wrap_err("CL genesis (genesis.ssz) was not produced")?;
    drop(container);

    // The generator bakes the validators into `genesis.ssz` from the mnemonic but does not emit
    // keystores. Recover the matching keystores (same mnemonic, EIP-2334 derivation) so the
    // validator client can sign and produce blocks. Written to `<data>/vc/{validators,secrets}`.
    recover_validator_keys(config, &data_dir).await?;

    Ok(data_dir)
}

/// Mnemonic shared by the CL genesis (`EL_AND_CL_MNEMONIC`) and the recovered validator
/// keystores — must match so the VC's validators are in the genesis active set.
const DEVNET_MNEMONIC: &str = "test test test test test test test test test test test junk";
/// Password applied to the recovered keystores (devnet-only).
const KEYSTORE_PASSWORD: &str = "devnet-keystore-password";
/// Fee recipient for L1 blocks the devnet validator proposes. Lighthouse requires a configured
/// fee recipient to produce post-merge blocks; the value is irrelevant on a throwaway devnet.
const SUGGESTED_FEE_RECIPIENT: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

/// Derives the validator keystores from [`DEVNET_MNEMONIC`] via `lighthouse account validator
/// recover`, writing `<data>/vc/validators` and `<data>/vc/secrets`.
async fn recover_validator_keys(config: &L1StackConfig, data_dir: &std::path::Path) -> Result<()> {
    let mnemonic_path = data_dir.join("mnemonic.txt");
    fs::write(&mnemonic_path, DEVNET_MNEMONIC)
        .await
        .wrap_err("failed to write validator mnemonic")?;
    fs::create_dir_all(data_dir.join("vc").join("secrets"))
        .await
        .wrap_err("failed to create validator keystore dirs")?;

    let image = format!("{}:{}", config.lighthouse_image, config.lighthouse_tag);
    let mut child = tokio::process::Command::new("docker")
        .args([
            "run",
            "--rm",
            "-i",
            "-v",
            &format!("{}:/keys", data_dir.to_string_lossy()),
            "--entrypoint",
            "lighthouse",
            &image,
            "account",
            "validator",
            "recover",
            "--mnemonic-path",
            "/keys/mnemonic.txt",
            "--first-index",
            "0",
            "--count",
            "1",
            "--datadir",
            "/keys/vc",
            "--secrets-dir",
            "/keys/vc/secrets",
            "--stdin-inputs",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .wrap_err("failed to spawn lighthouse validator recovery")?;

    if let Some(mut stdin) = child.stdin.take() {
        // `--stdin-inputs` expects the keystore password entered twice.
        let input = format!("{KEYSTORE_PASSWORD}\n{KEYSTORE_PASSWORD}\n");
        stdin
            .write_all(input.as_bytes())
            .await
            .wrap_err("failed to feed keystore password")?;
    }

    let output = child
        .wait_with_output()
        .await
        .wrap_err("lighthouse validator recovery did not complete")?;
    if !output.status.success() {
        return Err(eyre!(
            "lighthouse validator recovery failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    info!("recovered L1 validator keystores from mnemonic");
    Ok(())
}

/// Starts the reth L1 execution layer. Returns the container, its host RPC URL, and the host port
/// the Engine API (authrpc) is mapped to.
async fn start_reth(config: &L1StackConfig) -> Result<(ContainerAsync<GenericImage>, String, u16)> {
    let genesis_dir = config
        .el_genesis_path
        .parent()
        .ok_or_else(|| eyre!("EL genesis path has no parent dir"))?;
    let genesis_name = config
        .el_genesis_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| eyre!("EL genesis path has no file name"))?;
    let jwt_dir = config
        .jwt_path
        .parent()
        .ok_or_else(|| eyre!("JWT path has no parent dir"))?;
    let jwt_name = config
        .jwt_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| eyre!("JWT path has no file name"))?;

    let image = GenericImage::new(config.reth_image.clone(), config.reth_tag.clone())
        .with_entrypoint("reth")
        .with_exposed_port(RETH_RPC_PORT.tcp())
        .with_exposed_port(RETH_ENGINE_PORT.tcp())
        .with_wait_for(WaitFor::message_on_stdout("RPC HTTP server started"))
        .with_log_consumer(container_log_consumer(
            "l1-reth",
            ProcessLogTarget::L1DevChain,
        ));

    let cmd = vec![
        "node".to_string(),
        "--chain".to_string(),
        format!("/genesis/{genesis_name}"),
        "--datadir".to_string(),
        "/data".to_string(),
        "--http".to_string(),
        "--http.addr".to_string(),
        "0.0.0.0".to_string(),
        "--http.port".to_string(),
        RETH_RPC_PORT.to_string(),
        "--http.api".to_string(),
        "admin,eth,web3,net,rpc,debug,txpool".to_string(),
        "--authrpc.addr".to_string(),
        "0.0.0.0".to_string(),
        "--authrpc.port".to_string(),
        RETH_ENGINE_PORT.to_string(),
        "--authrpc.jwtsecret".to_string(),
        format!("/jwt/{jwt_name}"),
        "--disable-discovery".to_string(),
        "-vvv".to_string(),
    ];

    let mut request = image
        .with_cmd(cmd)
        .with_mount(Mount::bind_mount(
            genesis_dir.to_string_lossy().to_string(),
            "/genesis",
        ))
        .with_mount(Mount::bind_mount(
            jwt_dir.to_string_lossy().to_string(),
            "/jwt",
        ))
        .with_startup_timeout(Duration::from_secs(120));

    if let Some(port) = config.stable_rpc_port {
        request = request.with_mapped_port(port, RETH_RPC_PORT.tcp());
    }

    let container = request.start().await.wrap_err("reth L1 failed to start")?;
    let host = container.get_host().await?;
    let rpc_port = container.get_host_port_ipv4(RETH_RPC_PORT.tcp()).await?;
    let engine_port = container.get_host_port_ipv4(RETH_ENGINE_PORT.tcp()).await?;
    let rpc_url = format!("http://{host}:{rpc_port}");

    Ok((container, rpc_url, engine_port))
}

/// Starts the Lighthouse beacon node, wired to reth's Engine API via the host bridge.
async fn start_beacon(
    config: &L1StackConfig,
    data_dir: &std::path::Path,
    engine_host_port: u16,
) -> Result<(ContainerAsync<GenericImage>, String)> {
    let jwt_dir = config
        .jwt_path
        .parent()
        .ok_or_else(|| eyre!("JWT path has no parent dir"))?;
    let jwt_name = config
        .jwt_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| eyre!("JWT path has no file name"))?;

    let image = GenericImage::new(
        config.lighthouse_image.clone(),
        config.lighthouse_tag.clone(),
    )
    .with_entrypoint("lighthouse")
    .with_exposed_port(LIGHTHOUSE_BEACON_PORT.tcp())
    .with_wait_for(WaitFor::message_on_stdout("HTTP API started"))
    .with_log_consumer(container_log_consumer(
        "l1-beacon",
        ProcessLogTarget::L1DevChain,
    ));

    let cmd = vec![
        "bn".to_string(),
        "--testnet-dir".to_string(),
        "/l1/metadata".to_string(),
        "--datadir".to_string(),
        "/data/beacon".to_string(),
        "--enable-private-discovery".to_string(),
        "--disable-peer-scoring".to_string(),
        "--staking".to_string(),
        "--http".to_string(),
        "--http-address".to_string(),
        "0.0.0.0".to_string(),
        "--http-port".to_string(),
        LIGHTHOUSE_BEACON_PORT.to_string(),
        "--http-allow-origin".to_string(),
        "*".to_string(),
        "--target-peers".to_string(),
        "0".to_string(),
        "--execution-endpoint".to_string(),
        format!("http://{DOCKER_HOST}:{engine_host_port}"),
        "--execution-jwt".to_string(),
        format!("/jwt/{jwt_name}"),
    ];

    let mut request = image
        .with_cmd(cmd)
        .with_mount(Mount::bind_mount(
            data_dir.to_string_lossy().to_string(),
            "/l1",
        ))
        .with_mount(Mount::bind_mount(
            jwt_dir.to_string_lossy().to_string(),
            "/jwt",
        ))
        .with_startup_timeout(Duration::from_secs(120));

    if let Some(port) = config.stable_beacon_port {
        request = request.with_mapped_port(port, LIGHTHOUSE_BEACON_PORT.tcp());
    }

    let container = request
        .start()
        .await
        .wrap_err("Lighthouse beacon failed to start")?;
    let host = container.get_host().await?;
    let port = container
        .get_host_port_ipv4(LIGHTHOUSE_BEACON_PORT.tcp())
        .await?;
    let beacon_url = format!("http://{host}:{port}");

    Ok((container, beacon_url))
}

/// Starts the Lighthouse validator client, wired to the beacon node via the host bridge.
async fn start_validator(
    config: &L1StackConfig,
    data_dir: &std::path::Path,
    beacon_host_port: u16,
) -> Result<ContainerAsync<GenericImage>> {
    let image = GenericImage::new(
        config.lighthouse_image.clone(),
        config.lighthouse_tag.clone(),
    )
    .with_entrypoint("lighthouse")
    .with_wait_for(WaitFor::seconds(3))
    .with_log_consumer(container_log_consumer(
        "l1-validator",
        ProcessLogTarget::L1DevChain,
    ));

    let cmd = vec![
        "vc".to_string(),
        "--testnet-dir".to_string(),
        "/l1/metadata".to_string(),
        "--validators-dir".to_string(),
        "/l1/vc/validators".to_string(),
        "--secrets-dir".to_string(),
        "/l1/vc/secrets".to_string(),
        "--beacon-nodes".to_string(),
        format!("http://{DOCKER_HOST}:{beacon_host_port}"),
        "--init-slashing-protection".to_string(),
        // Post-merge block production requires a fee recipient; without it Lighthouse aborts
        // proposing ("Fee recipient unknown") and the L1 never advances past genesis.
        "--suggested-fee-recipient".to_string(),
        SUGGESTED_FEE_RECIPIENT.to_string(),
    ];

    let request = image
        .with_cmd(cmd)
        .with_mount(Mount::bind_mount(
            data_dir.to_string_lossy().to_string(),
            "/l1",
        ))
        .with_startup_timeout(Duration::from_secs(120));

    let container = request
        .start()
        .await
        .wrap_err("Lighthouse validator failed to start")?;
    Ok(container)
}

/// Extracts the host port from a `http://host:port` beacon URL.
fn beacon_host_port(beacon_url: &str) -> Result<u16> {
    beacon_url
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| eyre!("could not parse port from beacon url {beacon_url}"))
}

/// Polls until `path` exists or the timeout elapses.
async fn wait_for_file(path: &std::path::Path, timeout: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if fs::try_exists(path).await.unwrap_or(false) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(eyre!("timed out waiting for {}", path.display()))
}
