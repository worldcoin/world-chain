use std::{
    fs,
    io::Read,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use alloy_eips::{BlockNumberOrTag, eip1559::BaseFeeParams};
use alloy_genesis::Genesis;
use alloy_primitives::{B64, hex};
use alloy_provider::{Provider, ProviderBuilder};
use base64::prelude::{BASE64_STANDARD, Engine};
use eyre::eyre::{Context, Result, bail, eyre};
use flate2::read::GzDecoder;
use futures::future::try_join_all;
use op_alloy_consensus::{encode_holocene_extra_data, encode_jovian_extra_data};
use rand::Rng as _;
use reth_chainspec::EthChainSpec;
use reth_network_peers::{NodeRecord, TrustedPeer};
use secp256k1::SecretKey;
use serde_json::{Value, json};
use tempfile::TempDir;
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{IntoContainerPort, Mount, WaitFor},
    runners::AsyncRunner,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    task::JoinHandle,
};
use tracing::{debug, info};
use url::{Host, Url};
use world_chain_chainspec::{WorldChainHardfork, WorldChainSpec};
use world_chain_test_utils::DEV_CHAIN_ID;

use crate::{
    DevnetComponent, DevnetComponentKind, DevnetComponentStatus, DevnetPortMode, L1DevChain,
    L1DevChainConfig, MetricsTarget, ObservabilityStack, WorldChainHardforkConfig,
    component::ContainerImage,
    op_stack::{HaSequencerConfig, HaSequencerTopology},
    process_logs::{ProcessLogTarget, container_log_consumer, emit_process_log},
};

const ANVIL_RPC_PORT: u16 = 8545;
const OP_BATCHER_MAX_CHANNEL_DURATION_L1_BLOCKS: &str = "4";
const OP_PROPOSER_PERMISSIONED_GAME_TYPE: &str = "1";
const OP_TXMGR_NETWORK_TIMEOUT: &str = "30s";
const OP_TXMGR_RESUBMISSION_TIMEOUT: &str = "5m";
const CONDUCTOR_RPC_PORT: u16 = 8545;
const CONDUCTOR_WS_PORT: u16 = 8546;
const CONDUCTOR_CONSENSUS_PORT: u16 = 50050;
const CONDUCTOR_METRICS_PORT: u16 = 7300;
const CONDUCTOR_HEALTHCHECK_INTERVAL_SECS: &str = "5";
const CONDUCTOR_HEALTHCHECK_UNSAFE_INTERVAL_SECS: &str = "300";
const SERVICE_RPC_PORT: u16 = 8545;
const SERVICE_METRICS_PORT: u16 = 7300;

const DEVNET_PRIVATE_KEY: &str =
    "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d";
const UNSAFE_BLOCK_SIGNER_PRIVATE_KEY: &str =
    "0x92db14e403b83dfe3df233f83dfa3a0d7096f21ca9b0d6d6b8d88b2b4ec1564e";
/// Address of [`UNSAFE_BLOCK_SIGNER_PRIVATE_KEY`], matching `unsafeBlockSigner` in the deployer
/// intent. Passed to every node so the kona P2P gossip layer expects the correct unsafe-block
/// signer immediately, rather than defaulting to the zero address before the L1 system config is
/// fetched (which makes followers reject the leader's gossiped unsafe blocks).
const UNSAFE_BLOCK_SIGNER_ADDRESS: &str = "0x976EA74026E726554dB657fA54763abd0C3a0aa9";
const BATCHER_PRIVATE_KEY: &str =
    "0x4bbbf85ce3377467afe5d46f804f221813b2bb87f24d81f60f1fcdbf7cbf4356";
const PROPOSER_PRIVATE_KEY: &str =
    "0xdbda1821b80551c9d65939329250298aa3472ba22feea921c0cf5d620ea67b97";
const CHALLENGER_PRIVATE_KEY: &str =
    "0x2a871d0798f97d79848a013d4936a73bf4cc922c825d33c1cf7073dff6d409c6";

const FLASHBLOCKS_BUILDER_KEYS: [&str; 3] = [
    "40645f645e9e28a3f00637d8d629736e7934ee857154ec3fd336c3cc014ebb62",
    "2bf67f0541606bbffe221c9f00d1d5eddba777c2caa9e2171eae6a2100fe2f70",
    "09dba52ebb77d2981aa41f0206cfff58d42ef02918e3c5c396fb74ba7ae7e51b",
];
const FLASHBLOCKS_DEV_AUTHORIZER_SK: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";
const JWT_SECRET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const PBH_DISABLED_ENTRYPOINT: &str = "0x000000000000000000000000000000000000dEaD";
const PBH_DISABLED_WORLD_ID: &str = "0x000000000000000000000000000000000000dEa1";
const PBH_DISABLED_SIGNATURE_AGGREGATOR: &str = "0x000000000000000000000000000000000000dEa2";

#[derive(Debug)]
pub struct FullStackWorldDevnet {
    _batcher: Option<ContainerService>,
    _proposer: Option<ContainerService>,
    _challenger: Option<ContainerService>,
    _conductors: Vec<ConductorService>,
    sequencers: Vec<SequencerService>,
    observability: Option<ObservabilityStack>,
    l1: L1DevChain,
    components: Vec<DevnetComponent>,
    removed_services: Vec<DevnetComponent>,
    _tempdir: TempDir,
}

#[derive(Debug)]
struct SequencerService {
    id: String,
    rpc_url: String,
    ws_url: String,
    auth_url: String,
    flashblocks_url: String,
    /// Native (host-reachable) URL of the in-process kona consensus node RPC.
    ///
    /// Serves `optimism_syncStatus`, `optimism_rollupConfig`, and the
    /// `admin_*Sequencer` admin namespace. Reachable from the host at
    /// `127.0.0.1:<kona_rpc_host_port>` and from containers via
    /// `host.docker.internal:<kona_rpc_host_port>`.
    kona_rpc_url: String,
    kona_rpc_host_port: u16,
    p2p_host_port: u16,
    kona_p2p_host_port: u16,
    metrics_target: MetricsTarget,
    binary: PathBuf,
    _process: NativeProcess,
}

#[derive(Clone, Debug)]
struct SequencerPlan {
    rpc_host_port: u16,
    ws_host_port: u16,
    auth_host_port: u16,
    metrics_host_port: u16,
    p2p_host_port: u16,
    p2p_secret_key: String,
    trusted_peer: String,
    /// Host port for the in-process kona consensus node RPC (native bind on
    /// `0.0.0.0`, directly reachable at `127.0.0.1:<port>`).
    kona_rpc_host_port: u16,
    /// Host port for the in-process kona consensus P2P listener.
    kona_p2p_host_port: u16,
    /// secp256k1 secret key (hex, no `0x`) for the kona consensus P2P identity.
    kona_p2p_secret_key: String,
    /// Consensus enode advertised to the other sequencers' kona P2P bootstore.
    kona_consensus_enode: String,
}

#[derive(Clone, Debug)]
struct ConductorPlan {
    server_id: String,
    rpc_url: String,
    ws_url: String,
    consensus_advertised: String,
    rpc_host_port: u16,
    ws_host_port: u16,
    consensus_host_port: u16,
    metrics_host_port: u16,
    metrics_target: MetricsTarget,
}

#[derive(Debug)]
struct ConductorService {
    id: String,
    server_id: String,
    rpc_url: String,
    ws_url: String,
    consensus_advertised: String,
    metrics_target: MetricsTarget,
    image: ContainerImage,
    _container: ContainerAsync<GenericImage>,
}

#[derive(Debug)]
struct ContainerService {
    id: String,
    kind: DevnetComponentKind,
    rpc_url: Option<String>,
    metrics_target: Option<MetricsTarget>,
    image: ContainerImage,
    _container: ContainerAsync<GenericImage>,
}

#[derive(Debug)]
struct NativeProcess {
    id: String,
    child: Child,
    _log_tasks: Vec<JoinHandle<()>>,
}

impl Drop for NativeProcess {
    fn drop(&mut self) {
        if let Err(err) = self.child.start_kill() {
            debug!(
                id = %self.id,
                %err,
                "failed to signal native devnet process during cleanup"
            );
        }
    }
}

#[derive(Debug)]
struct OpArtifacts {
    workdir: TempDir,
    rollup_path: PathBuf,
    l1_genesis_path: PathBuf,
    l1_addresses: Value,
}

impl FullStackWorldDevnet {
    pub async fn start(
        config: HaSequencerConfig,
        hardforks: WorldChainHardforkConfig,
        port_mode: DevnetPortMode,
        block_time: Duration,
        access_list: bool,
    ) -> Result<Self> {
        let topology = HaSequencerTopology::from_config(config.clone());
        let artifacts = generate_op_artifacts(&config, &hardforks).await?;
        let workdir_path = artifacts.workdir.path().to_path_buf();

        let mut l1_config = L1DevChainConfig {
            block_time_secs: block_time.as_secs().max(1),
            genesis_file: Some(artifacts.l1_genesis_path.clone()),
            ..L1DevChainConfig::default()
        };
        if port_mode == DevnetPortMode::Stable {
            l1_config.stable_port = Some(ANVIL_RPC_PORT);
        }

        let l1 = L1DevChain::start(l1_config)
            .await
            .wrap_err("failed to start OP-contract-backed L1 dev chain")?;
        let l1_public_rpc = l1.rpc_url().to_string();
        let l1_internal_rpc = host_internal_url(l1.rpc_url())?;
        let actual_l1_hash = block_hash(&l1_public_rpc, 0)
            .await
            .wrap_err("failed to read Anvil L1 genesis hash")?;
        patch_rollup_l1_hash(&artifacts.rollup_path, &actual_l1_hash)?;

        info!(
            l1_rpc_url = %l1_public_rpc,
            l1_genesis_hash = %actual_l1_hash,
            "L1 dev chain has OP contracts loaded"
        );

        let sequencer_count = config.sequencer_count.max(1) as usize;
        let sequencer_plans = (0..sequencer_count)
            .map(plan_sequencer)
            .collect::<Result<Vec<_>>>()
            .wrap_err("failed to plan world-chain EL peer mesh")?;

        // Plan conductors before starting the sequencers: each monomorphic
        // sequencer runs kona in-process and needs its conductor's RPC host port
        // up front (kona's outbound `--kona.conductor.rpc` client). The conductor
        // containers themselves are still started afterwards.
        let mut conductor_plans = Vec::with_capacity(sequencer_count);
        for index in 0..sequencer_count {
            conductor_plans.push(plan_conductor(index, port_mode)?);
        }

        let rollup_config_path = workdir_path.join("rollup.json");
        let l1_slot_duration_secs = block_time.as_secs().max(1);
        let trusted_peers = sequencer_plans
            .iter()
            .map(|plan| plan.trusted_peer.clone())
            .collect::<Vec<_>>();
        let sequencers = try_join_all((0..sequencer_count).map(|index| {
            let trusted_peers = trusted_peers
                .iter()
                .enumerate()
                .filter_map(|(peer_index, peer)| (peer_index != index).then_some(peer.clone()))
                .collect::<Vec<_>>();
            let kona_consensus_bootnodes = kona_consensus_bootnodes(&sequencer_plans, index);
            start_world_chain_el(
                index,
                &workdir_path,
                &sequencer_plans[index],
                trusted_peers,
                WorldChainElKona {
                    rollup_config_path: &rollup_config_path,
                    l1_rpc_url: &l1_public_rpc,
                    l1_slot_duration_secs,
                    conductor_rpc_host_port: conductor_plans[index].rpc_host_port,
                    consensus_bootnodes: kona_consensus_bootnodes,
                },
            )
        }))
        .await?;
        connect_execution_peers(&sequencers).await?;
        wait_for_kona_consensus_mesh(&sequencers).await?;

        let mut conductors = Vec::with_capacity(sequencer_count);
        conductors.push(
            start_conductor(
                0,
                sequencer_count,
                &config.images.op_conductor,
                &workdir_path,
                &sequencers[0],
                &conductor_plans[0],
            )
            .await?,
        );

        wait_for_conductor_leader(&conductors[0], Duration::from_secs(90)).await?;
        start_bootstrap_sequencer(&sequencers[0]).await?;
        wait_for_l2_blocks_with_logs(
            &sequencers[0].rpc_url,
            1,
            Duration::from_secs(120),
            &sequencers,
            &conductors,
        )
        .await?;

        for index in 1..sequencer_count {
            conductors.push(
                start_conductor(
                    index,
                    sequencer_count,
                    &config.images.op_conductor,
                    &workdir_path,
                    &sequencers[index],
                    &conductor_plans[index],
                )
                .await?,
            );
        }

        configure_conductor_cluster(&conductors).await?;
        wait_for_conductor_health(&conductors).await?;
        wait_for_l2_blocks_with_logs(
            &sequencers[0].rpc_url,
            2,
            Duration::from_secs(120),
            &sequencers,
            &conductors,
        )
        .await?;

        let conductor_rpc_internal = host_internal_url(&conductors[0].rpc_url)?;
        let l2_rpc_internal = host_internal_url(&sequencers[0].rpc_url)?;
        let game_factory = l1_address(&artifacts.l1_addresses, "DisputeGameFactoryProxy")?;

        let (batcher, proposer, challenger) = if config.op_challenger {
            let (batcher, proposer, challenger) = tokio::try_join!(
                start_batcher(
                    &config.images.op_batcher,
                    &l1_internal_rpc,
                    &conductor_rpc_internal,
                ),
                start_proposer(
                    &config.images.op_proposer,
                    &l1_internal_rpc,
                    &conductor_rpc_internal,
                    &game_factory,
                ),
                start_challenger(
                    &config.images.op_challenger,
                    &workdir_path,
                    &l1_internal_rpc,
                    &l2_rpc_internal,
                    &conductor_rpc_internal,
                    &game_factory,
                ),
            )?;
            (Some(batcher), Some(proposer), Some(challenger))
        } else {
            let (batcher, proposer) = tokio::try_join!(
                start_batcher(
                    &config.images.op_batcher,
                    &l1_internal_rpc,
                    &conductor_rpc_internal,
                ),
                start_proposer(
                    &config.images.op_proposer,
                    &l1_internal_rpc,
                    &conductor_rpc_internal,
                    &game_factory,
                ),
            )?;
            (Some(batcher), Some(proposer), None)
        };

        let mut metrics_targets = Vec::new();
        metrics_targets.extend(
            sequencers
                .iter()
                .map(|service| service.metrics_target.clone()),
        );
        metrics_targets.extend(
            conductors
                .iter()
                .map(|service| service.metrics_target.clone()),
        );
        for service in [&batcher, &proposer, &challenger].into_iter().flatten() {
            if let Some(target) = &service.metrics_target {
                metrics_targets.push(target.clone());
            }
        }

        let observability =
            ObservabilityStack::start(config.observability.clone(), metrics_targets)
                .await
                .wrap_err("failed to start full-stack Prometheus/Grafana")?;

        let components = build_components(
            &config,
            &l1_public_rpc,
            &sequencers,
            &conductors,
            batcher.as_ref(),
            proposer.as_ref(),
            challenger.as_ref(),
            observability.as_ref(),
            &game_factory,
        );

        Ok(Self {
            _batcher: batcher,
            _proposer: proposer,
            _challenger: challenger,
            _conductors: conductors,
            sequencers,
            observability,
            l1,
            components,
            removed_services: topology.removed_services,
            _tempdir: artifacts.workdir,
        })
    }

    pub fn l1_rpc_url(&self) -> &str {
        self.l1.rpc_url()
    }

    pub fn l2_rpc_url(&self) -> &str {
        &self.sequencers[0].rpc_url
    }

    pub fn sequencer_rpc_url(&self) -> &str {
        &self.sequencers[0].rpc_url
    }

    pub fn flashblocks_url(&self) -> &str {
        &self.sequencers[0].flashblocks_url
    }

    pub async fn safe_block_number(&self) -> Result<u64> {
        let sequencer = self
            .sequencers
            .first()
            .ok_or_else(|| eyre!("full-stack devnet has no sequencer"))?;
        let sync_status =
            json_rpc(&sequencer.kona_rpc_url, "optimism_syncStatus", json!([])).await?;
        let safe_number = sync_status
            .pointer("/safe_l2/number")
            .ok_or_else(|| eyre!("optimism_syncStatus missing safe_l2.number: {sync_status}"))?;

        json_rpc_quantity_to_u64(safe_number)
    }

    pub fn prometheus_url(&self) -> Option<&str> {
        self.observability
            .as_ref()
            .map(ObservabilityStack::prometheus_url)
    }

    pub fn grafana_url(&self) -> Option<&str> {
        self.observability
            .as_ref()
            .map(ObservabilityStack::grafana_url)
    }

    pub fn components(&self) -> Vec<DevnetComponent> {
        let mut components = self.components.clone();
        components.extend(self.removed_services.clone());
        components
    }

    pub async fn wait_ready(&self) -> Result<()> {
        wait_for_l2_blocks(self.l2_rpc_url(), 1, Duration::from_secs(90)).await
    }
}

async fn generate_op_artifacts(
    config: &HaSequencerConfig,
    hardforks: &WorldChainHardforkConfig,
) -> Result<OpArtifacts> {
    let workdir = tempfile::Builder::new()
        .prefix("world-devnet-op-")
        .tempdir()
        .wrap_err("failed to create OP deployer tempdir")?;
    let workdir_path = workdir.path();

    fs::write(workdir_path.join("jwt.hex"), JWT_SECRET)
        .wrap_err("failed to write Engine API JWT secret")?;
    fs::create_dir_all(workdir_path.join("prestates"))
        .wrap_err("failed to create op-challenger prestates directory")?;

    let image = config.images.op_deployer.reference();
    let mount = format!("{}:/work", workdir_path.display());

    run_docker(
        "op-deployer init",
        vec![
            "run".into(),
            "--rm".into(),
            "-v".into(),
            mount.clone(),
            "--entrypoint".into(),
            "op-deployer".into(),
            image.clone(),
            "init".into(),
            "--workdir".into(),
            "/work".into(),
            "--l1-chain-id".into(),
            "31337".into(),
            "--l2-chain-ids".into(),
            DEV_CHAIN_ID.to_string(),
            "--intent-type".into(),
            "custom".into(),
        ],
    )
    .await?;

    fs::write(workdir_path.join("intent.toml"), render_intent(config))
        .wrap_err("failed to write op-deployer intent.toml")?;

    run_docker(
        "op-deployer apply",
        vec![
            "run".into(),
            "--rm".into(),
            "-v".into(),
            mount.clone(),
            "--entrypoint".into(),
            "op-deployer".into(),
            image.clone(),
            "apply".into(),
            "--workdir".into(),
            "/work".into(),
            "--deployment-target".into(),
            "genesis".into(),
        ],
    )
    .await?;

    for (name, output, chain_id) in [
        ("genesis", "/work/genesis.json", DEV_CHAIN_ID.to_string()),
        ("rollup", "/work/rollup.json", DEV_CHAIN_ID.to_string()),
        ("l1", "/work/l1-addresses.json", DEV_CHAIN_ID.to_string()),
    ] {
        run_docker(
            &format!("op-deployer inspect {name}"),
            vec![
                "run".into(),
                "--rm".into(),
                "-v".into(),
                mount.clone(),
                "--entrypoint".into(),
                "op-deployer".into(),
                image.clone(),
                "inspect".into(),
                name.into(),
                "--workdir".into(),
                "/work".into(),
                "--outfile".into(),
                output.into(),
                chain_id,
            ],
        )
        .await?;
    }

    let state_path = workdir_path.join("state.json");
    let genesis_path = workdir_path.join("genesis.json");
    let rollup_path = workdir_path.join("rollup.json");
    let l1_addresses_path = workdir_path.join("l1-addresses.json");
    let l1_genesis_path = workdir_path.join("l1-genesis.json");

    patch_l2_hardforks(&genesis_path, &rollup_path, hardforks)
        .wrap_err("failed to patch generated L2 hardfork schedule")?;
    write_l1_genesis(&state_path, &l1_genesis_path)
        .wrap_err("failed to render L1 genesis from op-deployer state")?;

    let l1_addresses = read_json(&l1_addresses_path)?;

    info!(
        workdir = %workdir_path.display(),
        genesis = %genesis_path.display(),
        rollup = %rollup_path.display(),
        l1_addresses = %l1_addresses_path.display(),
        "generated OP Stack genesis deployment artifacts"
    );

    Ok(OpArtifacts {
        workdir,
        rollup_path,
        l1_genesis_path,
        l1_addresses,
    })
}

fn render_intent(config: &HaSequencerConfig) -> String {
    format!(
        r#"configType = "custom"
l1ChainID = 31337
fundDevAccounts = true
useInterop = false
l1ContractsLocator = "{}"
l2ContractsLocator = "{}"

[superchainRoles]
  SuperchainProxyAdminOwner = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8"
  SuperchainGuardian = "0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC"
  ProtocolVersionsOwner = "0x90F79bf6EB2c4f870365E785982E1f101E93b906"

[[chains]]
  id = "0x{:064x}"
  baseFeeVaultRecipient = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8"
  l1FeeVaultRecipient = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8"
  sequencerFeeVaultRecipient = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8"
  eip1559DenominatorCanyon = 250
  eip1559Denominator = 50
  eip1559Elasticity = 10
  operatorFeeScalar = 0
  operatorFeeConstant = 0
  [chains.roles]
    l1ProxyAdminOwner = "0x15d34AAf54267DB7D7c367839AAf71A00a2C6A65"
    l2ProxyAdminOwner = "0x15d34AAf54267DB7D7c367839AAf71A00a2C6A65"
    systemConfigOwner = "0x9965507D1a55bcC2695C58ba16FB37d819B0A4dc"
    unsafeBlockSigner = "0x976EA74026E726554dB657fA54763abd0C3a0aa9"
    batcher = "0x14dC79964da2C08b23698B3D3cc7Ca32193d9955"
    proposer = "0x23618e81E3f5cdF7f54C3d65f7FBc0aBf5B21E8f"
    challenger = "0xa0Ee7A142d267C1f36714E4a8F75612F20a79720"
"#,
        config.op_contracts.l1_artifacts_locator,
        config.op_contracts.l2_artifacts_locator,
        DEV_CHAIN_ID
    )
}

fn write_l1_genesis(state_path: &Path, output_path: &Path) -> Result<()> {
    let state = read_json(state_path)?;
    let encoded = state
        .get("l1StateDump")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("op-deployer state missing l1StateDump"))?;
    let compressed = BASE64_STANDARD
        .decode(encoded)
        .wrap_err("failed to decode base64 l1StateDump")?;
    let mut decoder = GzDecoder::new(compressed.as_slice());
    let mut alloc_json = String::new();
    decoder
        .read_to_string(&mut alloc_json)
        .wrap_err("failed to decompress l1StateDump")?;
    let alloc: Value = serde_json::from_str(&alloc_json).wrap_err("invalid l1StateDump JSON")?;
    let timestamp = state
        .pointer("/opChainDeployments/0/startBlock/timestamp")
        .and_then(Value::as_str)
        .unwrap_or("0x0")
        .to_string();

    let genesis = json!({
        "config": {
            "chainId": 31337,
            "homesteadBlock": 0,
            "daoForkSupport": false,
            "eip150Block": 0,
            "eip155Block": 0,
            "eip158Block": 0,
            "byzantiumBlock": 0,
            "constantinopleBlock": 0,
            "petersburgBlock": 0,
            "istanbulBlock": 0,
            "muirGlacierBlock": 0,
            "berlinBlock": 0,
            "londonBlock": 0,
            "arrowGlacierBlock": 0,
            "grayGlacierBlock": 0,
            "shanghaiTime": 0,
            "cancunTime": 0,
            "pragueTime": 0,
            "blobSchedule": {
                "cancun": {
                    "target": 3,
                    "max": 6,
                    "baseFeeUpdateFraction": 3338477
                },
                "prague": {
                    "target": 6,
                    "max": 9,
                    "baseFeeUpdateFraction": 5007716
                }
            },
            "mergeNetsplitBlock": 0,
            "terminalTotalDifficulty": 0
        },
        "nonce": "0x0",
        "timestamp": timestamp,
        "extraData": "0x",
        "gasLimit": "0x1c9c380",
        "difficulty": "0x0",
        "mixHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
        "coinbase": "0x0000000000000000000000000000000000000000",
        "number": "0x0",
        "gasUsed": "0x0",
        "parentHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
        "baseFeePerGas": "0x3b9aca00",
        "excessBlobGas": "0x0",
        "alloc": alloc
    });

    fs::write(output_path, serde_json::to_vec_pretty(&genesis)?)
        .wrap_err("failed to write l1-genesis.json")
}

fn patch_l2_hardforks(
    genesis_path: &Path,
    rollup_path: &Path,
    hardforks: &WorldChainHardforkConfig,
) -> Result<()> {
    let mut genesis = read_json(genesis_path)?;
    let mut rollup = read_json(rollup_path)?;
    let genesis_config = genesis
        .get_mut("config")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| eyre!("generated genesis missing config object"))?;
    let rollup_config = rollup
        .as_object_mut()
        .ok_or_else(|| eyre!("generated rollup config is not an object"))?;

    set_time(
        genesis_config,
        "regolithTime",
        hardforks.is_active(WorldChainHardfork::Regolith),
    );
    set_time(
        genesis_config,
        "canyonTime",
        hardforks.is_active(WorldChainHardfork::Canyon),
    );
    set_time(
        genesis_config,
        "ecotoneTime",
        hardforks.is_active(WorldChainHardfork::Ecotone),
    );
    set_time(
        genesis_config,
        "fjordTime",
        hardforks.is_active(WorldChainHardfork::Fjord),
    );
    set_time(
        genesis_config,
        "graniteTime",
        hardforks.is_active(WorldChainHardfork::Granite),
    );
    set_time(
        genesis_config,
        "holoceneTime",
        hardforks.is_active(WorldChainHardfork::Holocene),
    );
    set_time(
        genesis_config,
        "isthmusTime",
        hardforks.is_active(WorldChainHardfork::Isthmus),
    );
    set_time(
        genesis_config,
        "jovianTime",
        hardforks.is_active(WorldChainHardfork::Jovian),
    );
    set_time(
        genesis_config,
        "tropoTime",
        hardforks.is_active(WorldChainHardfork::Tropo),
    );
    set_time(
        genesis_config,
        "stratoTime",
        hardforks.is_active(WorldChainHardfork::Strato),
    );
    set_time(
        genesis_config,
        "shanghaiTime",
        hardforks.is_active(WorldChainHardfork::Canyon),
    );
    set_time(
        genesis_config,
        "cancunTime",
        hardforks.is_active(WorldChainHardfork::Ecotone),
    );
    set_time(
        genesis_config,
        "pragueTime",
        hardforks.is_active(WorldChainHardfork::Isthmus),
    );

    set_time(
        rollup_config,
        "regolith_time",
        hardforks.is_active(WorldChainHardfork::Regolith),
    );
    set_time(
        rollup_config,
        "canyon_time",
        hardforks.is_active(WorldChainHardfork::Canyon),
    );
    set_time(
        rollup_config,
        "ecotone_time",
        hardforks.is_active(WorldChainHardfork::Ecotone),
    );
    set_time(
        rollup_config,
        "fjord_time",
        hardforks.is_active(WorldChainHardfork::Fjord),
    );
    set_time(
        rollup_config,
        "granite_time",
        hardforks.is_active(WorldChainHardfork::Granite),
    );
    set_time(
        rollup_config,
        "holocene_time",
        hardforks.is_active(WorldChainHardfork::Holocene),
    );
    set_time(
        rollup_config,
        "isthmus_time",
        hardforks.is_active(WorldChainHardfork::Isthmus),
    );
    set_time(
        rollup_config,
        "jovian_time",
        hardforks.is_active(WorldChainHardfork::Jovian),
    );
    set_time(
        rollup_config,
        "tropo_time",
        hardforks.is_active(WorldChainHardfork::Tropo),
    );
    set_time(
        rollup_config,
        "strato_time",
        hardforks.is_active(WorldChainHardfork::Strato),
    );

    patch_l2_genesis_base_fee_extra_data(&mut genesis, rollup_config, hardforks)?;
    let l2_genesis_hash = l2_genesis_hash(&genesis)?;
    rollup_config
        .get_mut("genesis")
        .and_then(Value::as_object_mut)
        .and_then(|genesis| genesis.get_mut("l2"))
        .and_then(Value::as_object_mut)
        .and_then(|l2| l2.get_mut("hash"))
        .ok_or_else(|| eyre!("rollup config missing genesis.l2.hash"))?
        .clone_from(&Value::String(l2_genesis_hash));

    fs::write(genesis_path, serde_json::to_vec_pretty(&genesis)?)
        .wrap_err("failed to write patched L2 genesis")?;
    fs::write(rollup_path, serde_json::to_vec_pretty(&rollup)?)
        .wrap_err("failed to write patched rollup config")?;
    Ok(())
}

fn patch_l2_genesis_base_fee_extra_data(
    genesis: &mut Value,
    rollup_config: &serde_json::Map<String, Value>,
    hardforks: &WorldChainHardforkConfig,
) -> Result<()> {
    let extra_data = if hardforks.is_active(WorldChainHardfork::Jovian) {
        encode_jovian_extra_data(B64::ZERO, l2_base_fee_params(rollup_config)?, 0)
            .wrap_err("failed to encode Jovian genesis extraData")?
    } else if hardforks.is_active(WorldChainHardfork::Holocene) {
        encode_holocene_extra_data(B64::ZERO, l2_base_fee_params(rollup_config)?)
            .wrap_err("failed to encode Holocene genesis extraData")?
    } else {
        return Ok(());
    };

    genesis
        .as_object_mut()
        .ok_or_else(|| eyre!("generated genesis is not an object"))?
        .insert(
            "extraData".to_string(),
            Value::String(format!("0x{}", hex::encode(extra_data.as_ref()))),
        );
    Ok(())
}

fn l2_base_fee_params(rollup_config: &serde_json::Map<String, Value>) -> Result<BaseFeeParams> {
    let chain_op_config = rollup_config
        .get("chain_op_config")
        .and_then(Value::as_object)
        .ok_or_else(|| eyre!("rollup config missing chain_op_config"))?;
    let denominator = chain_op_config
        .get("eip1559DenominatorCanyon")
        .or_else(|| chain_op_config.get("eip1559Denominator"))
        .and_then(Value::as_u64)
        .ok_or_else(|| eyre!("rollup chain_op_config missing EIP-1559 denominator"))?;
    let elasticity = chain_op_config
        .get("eip1559Elasticity")
        .and_then(Value::as_u64)
        .ok_or_else(|| eyre!("rollup chain_op_config missing EIP-1559 elasticity"))?;

    Ok(BaseFeeParams::new(denominator.into(), elasticity.into()))
}

fn l2_genesis_hash(genesis: &Value) -> Result<String> {
    let genesis: Genesis = serde_json::from_value(genesis.clone())
        .wrap_err("failed to parse patched L2 genesis for hash derivation")?;
    let spec = WorldChainSpec::from_genesis(genesis);
    Ok(format!("{:#x}", spec.genesis_hash()))
}

fn set_time(map: &mut serde_json::Map<String, Value>, key: &str, active: bool) {
    if active {
        map.insert(key.to_string(), Value::from(0));
    } else {
        map.remove(key);
    }
}

fn patch_rollup_l1_hash(rollup_path: &Path, hash: &str) -> Result<()> {
    let mut rollup = read_json(rollup_path)?;
    let value = rollup
        .pointer_mut("/genesis/l1/hash")
        .ok_or_else(|| eyre!("rollup config missing genesis.l1.hash"))?;
    *value = Value::String(hash.to_string());
    fs::write(rollup_path, serde_json::to_vec_pretty(&rollup)?)
        .wrap_err("failed to write rollup config with actual L1 genesis hash")
}

fn plan_sequencer(_index: usize) -> Result<SequencerPlan> {
    let p2p_secret_key = random_p2p_secret_key();
    let p2p_host_port = reserve_host_port()?;
    let trusted_peer = devnet_enode(&p2p_secret_key, p2p_host_port)?;

    let kona_p2p_secret_key = random_p2p_secret_key();
    let kona_p2p_host_port = reserve_host_port()?;
    // The in-process kona consensus node advertises 127.0.0.1: it is native, so
    // its peers (the other native sequencers) reach it directly on the host.
    let kona_consensus_enode =
        devnet_trusted_peer(&kona_p2p_secret_key, "127.0.0.1", kona_p2p_host_port)?;

    Ok(SequencerPlan {
        rpc_host_port: reserve_host_port()?,
        ws_host_port: reserve_host_port()?,
        auth_host_port: reserve_host_port()?,
        metrics_host_port: reserve_host_port()?,
        p2p_host_port,
        p2p_secret_key,
        trusted_peer,
        kona_rpc_host_port: reserve_host_port()?,
        kona_p2p_host_port,
        kona_p2p_secret_key,
        kona_consensus_enode,
    })
}

/// Returns the consensus enodes of every sequencer except `source_index`, used
/// to seed the in-process kona P2P bootstore so the sequencer mesh forms.
fn kona_consensus_bootnodes(plans: &[SequencerPlan], source_index: usize) -> Vec<String> {
    plans
        .iter()
        .enumerate()
        .filter(|&(target_index, _)| target_index != source_index)
        .map(|(_, plan)| plan.kona_consensus_enode.clone())
        .collect()
}

/// Inputs the monomorphic world-chain client needs to run kona in-process as
/// the consensus/sequencer.
///
/// All URLs and paths here are addressed from the NATIVE world-chain process:
/// host-reachable URLs (the public Anvil L1, the host-mapped conductor RPC) and
/// native filesystem paths (the workdir rollup config), never the `/work` mount
/// or `host.docker.internal` used by the OP Stack containers.
struct WorldChainElKona<'a> {
    /// Native path to the patched rollup config (`<workdir>/rollup.json`).
    rollup_config_path: &'a Path,
    /// Host-reachable public Anvil L1 RPC (also used as the L1 beacon endpoint).
    l1_rpc_url: &'a str,
    /// L1 slot duration override, mirroring the dev L1 block time.
    l1_slot_duration_secs: u64,
    /// Host port of this sequencer's op-conductor RPC (kona's outbound client
    /// reaches it at `http://127.0.0.1:<port>` since the binary is native).
    conductor_rpc_host_port: u16,
    /// Consensus enodes of the other sequencers for the kona P2P bootstore.
    consensus_bootnodes: Vec<String>,
}

async fn start_world_chain_el(
    index: usize,
    workdir: &Path,
    plan: &SequencerPlan,
    trusted_peers: Vec<String>,
    kona: WorldChainElKona<'_>,
) -> Result<SequencerService> {
    let data_dir = workdir.join(format!("l2data-{index}"));
    fs::create_dir_all(&data_dir).wrap_err("failed to create L2 data dir")?;
    let binary = world_chain_binary()?;
    let genesis = workdir.join("genesis.json");
    let jwt = workdir.join("jwt.hex");

    run_native_command(
        &format!("world-chain init sequencer {index}"),
        &binary,
        &[
            "init".into(),
            "--chain".into(),
            genesis.to_string_lossy().to_string(),
            "--datadir".into(),
            data_dir.to_string_lossy().to_string(),
            "--log.stdout.format".into(),
            "log-fmt".into(),
            "-vvv".into(),
        ],
    )
    .await?;

    let builder_key = FLASHBLOCKS_BUILDER_KEYS[index % FLASHBLOCKS_BUILDER_KEYS.len()];
    let rpc_port = plan.rpc_host_port;
    let ws_port = plan.ws_host_port;
    let auth_port = plan.auth_host_port;
    let metrics_port = plan.metrics_host_port;
    let p2p_port = plan.p2p_host_port;
    let genesis_arg = genesis.to_string_lossy().to_string();
    let data_dir_arg = data_dir.to_string_lossy().to_string();
    let jwt_arg = jwt.to_string_lossy().to_string();
    let rpc_port_arg = rpc_port.to_string();
    let ws_port_arg = ws_port.to_string();
    let auth_port_arg = auth_port.to_string();
    let p2p_port_arg = p2p_port.to_string();
    let metrics_arg = format!("0.0.0.0:{metrics_port}");
    let p2p_secret_key = plan.p2p_secret_key.clone();
    // Each node needs a distinct IPC socket: reth's default `--ipcpath` is a global path
    // (`/tmp/reth.ipc`), so co-located nodes would collide. The in-process kona client connects to
    // reth's standard (non-engine) RPC over this IPC endpoint.
    let ipc_path_arg = data_dir.join("reth.ipc").to_string_lossy().to_string();
    let mut args = vec![
        "node".to_string(),
        "--chain".to_string(),
        genesis_arg,
        "--datadir".to_string(),
        data_dir_arg,
        "--port".to_string(),
        p2p_port_arg,
        "--p2p-secret-key-hex".to_string(),
        p2p_secret_key,
        "--no-persist-peers".to_string(),
        "--ipcpath".to_string(),
        ipc_path_arg,
        "--http".to_string(),
        "--http.addr".to_string(),
        "0.0.0.0".to_string(),
        "--http.port".to_string(),
        rpc_port_arg,
        "--http.api".to_string(),
        "admin,net,eth,web3,debug,trace,miner".to_string(),
        "--ws".to_string(),
        "--ws.addr".to_string(),
        "0.0.0.0".to_string(),
        "--ws.port".to_string(),
        ws_port_arg,
        "--ws.api".to_string(),
        "net,eth,miner".to_string(),
        "--authrpc.addr".to_string(),
        "0.0.0.0".to_string(),
        "--authrpc.port".to_string(),
        auth_port_arg,
        "--authrpc.jwtsecret".to_string(),
        jwt_arg,
        "--metrics".to_string(),
        metrics_arg,
        "--disable-discovery".to_string(),
    ];
    if !trusted_peers.is_empty() {
        args.extend(["--trusted-peers".to_string(), trusted_peers.join(",")]);
    }
    args.extend([
        "--builder.enabled".to_string(),
        "--builder.private-key".to_string(),
        DEVNET_PRIVATE_KEY.to_string(),
        "--builder.deadline".to_string(),
        "6".to_string(),
        "--builder.max-tasks".to_string(),
        "10".to_string(),
        "--pbh.verified-blockspace-capacity".to_string(),
        "0".to_string(),
        "--pbh.entrypoint".to_string(),
        PBH_DISABLED_ENTRYPOINT.to_string(),
        "--pbh.world-id".to_string(),
        PBH_DISABLED_WORLD_ID.to_string(),
        "--pbh.signature-aggregator".to_string(),
        PBH_DISABLED_SIGNATURE_AGGREGATOR.to_string(),
        "--flashblocks.enabled".to_string(),
        "--flashblocks.builder-sk".to_string(),
        builder_key.to_string(),
        "--flashblocks.override-authorizer-sk".to_string(),
        FLASHBLOCKS_DEV_AUTHORIZER_SK.to_string(),
        "--flashblocks.force-publish".to_string(),
        "--flashblocks.interval".to_string(),
        "200".to_string(),
        "--flashblocks.recommit-interval".to_string(),
        "20".to_string(),
        "--worldchain.disable-bootnodes".to_string(),
        "--log.stdout.format".to_string(),
        "log-fmt".to_string(),
        "-vvv".to_string(),
    ]);
    if access_list {
        args.push("--flashblocks.access-list".to_string());
    }

    // Monomorphic client: run kona in-process as the consensus/sequencer. When
    // `--kona.enabled` is set the binary auto-wires kona to reth's own launched
    // auth Engine API + JWT, so no engine URL/jwt is passed here.
    //
    // All addressing is NATIVE: the rollup config is the workdir path, the L1
    // and conductor URLs are host-reachable. The kona node RPC binds on
    // `0.0.0.0:<kona_rpc_host_port>` and is directly reachable at
    // `127.0.0.1:<port>` (and from containers at
    // `host.docker.internal:<port>`).
    let kona_rpc_port = plan.kona_rpc_host_port;
    let kona_p2p_port = plan.kona_p2p_host_port;
    let kona_rpc_port_arg = kona_rpc_port.to_string();
    let kona_p2p_port_arg = kona_p2p_port.to_string();
    let conductor_rpc_url = format!("http://127.0.0.1:{}", kona.conductor_rpc_host_port);
    let rollup_config_arg = kona.rollup_config_path.to_string_lossy().to_string();
    let kona_bootstore_arg = workdir
        .join(format!("kona-bootstore-{index}"))
        .to_string_lossy()
        .to_string();
    let kona_p2p_priv_arg = format!("0x{}", plan.kona_p2p_secret_key);
    args.extend([
        "--kona.enabled".to_string(),
        "--kona.sequencer".to_string(),
        // Start stopped: op-conductor starts the active leader's sequencer via
        // `admin_startSequencer` (see `start_bootstrap_sequencer`), mirroring the
        // old op-node `--sequencer.stopped` + bootstrap flow.
        "--kona.sequencer.stopped".to_string(),
        "--kona.sequencer.l1-confs".to_string(),
        "0".to_string(),
        "--kona.rollup-config".to_string(),
        rollup_config_arg,
        "--kona.l1-rpc-url".to_string(),
        kona.l1_rpc_url.to_string(),
        // Dev L1 (Anvil) has no separate beacon node; the EL RPC doubles as the
        // beacon endpoint, mirroring the old op-node `--l1-beacon = l1_rpc`.
        "--kona.l1-beacon-url".to_string(),
        kona.l1_rpc_url.to_string(),
        "--kona.l1-trust-rpc".to_string(),
        "--kona.l1-slot-duration-override".to_string(),
        kona.l1_slot_duration_secs.to_string(),
        "--kona.rpc.addr".to_string(),
        "0.0.0.0".to_string(),
        "--kona.rpc.port".to_string(),
        kona_rpc_port_arg,
        "--kona.rpc.enable-admin".to_string(),
        "--kona.conductor.rpc".to_string(),
        conductor_rpc_url,
        "--p2p.sequencer.key".to_string(),
        UNSAFE_BLOCK_SIGNER_PRIVATE_KEY.to_string(),
        "--p2p.unsafe.block.signer".to_string(),
        UNSAFE_BLOCK_SIGNER_ADDRESS.to_string(),
        "--p2p.listen.ip".to_string(),
        "0.0.0.0".to_string(),
        "--p2p.listen.tcp".to_string(),
        kona_p2p_port_arg.clone(),
        "--p2p.listen.udp".to_string(),
        kona_p2p_port_arg.clone(),
        "--p2p.advertise.ip".to_string(),
        "127.0.0.1".to_string(),
        "--p2p.advertise.tcp".to_string(),
        kona_p2p_port_arg.clone(),
        "--p2p.advertise.udp".to_string(),
        kona_p2p_port_arg,
        "--p2p.priv.raw".to_string(),
        kona_p2p_priv_arg,
        "--p2p.no-discovery".to_string(),
        "--p2p.bootstore".to_string(),
        kona_bootstore_arg,
    ]);
    if !kona.consensus_bootnodes.is_empty() {
        args.extend([
            "--p2p.bootnodes".to_string(),
            kona.consensus_bootnodes.join(","),
        ]);
    }

    let mut process = spawn_native_process(&format!("world-chain-el-{index}"), &binary, &args)
        .wrap_err_with(|| format!("failed to spawn native world-chain EL process {index}"))?;

    let rpc_url = format!("http://127.0.0.1:{rpc_port}");
    let ws_url = format!("ws://127.0.0.1:{ws_port}");
    let auth_url = format!("http://127.0.0.1:{auth_port}");

    wait_for_rpc_chain_id(&rpc_url, Duration::from_secs(90))
        .await
        .wrap_err_with(|| {
            let status = process.child.try_wait().ok().flatten();
            format!("world-chain EL {index} RPC did not become ready; process_status={status:?}")
        })?;

    // The in-process kona node RPC is native, so it is reachable at 127.0.0.1.
    let kona_rpc_url = format!("http://127.0.0.1:{kona_rpc_port}");
    wait_for_json_rpc(
        &kona_rpc_url,
        "optimism_rollupConfig",
        json!([]),
        Duration::from_secs(120),
    )
    .await
    .wrap_err_with(|| {
        let status = process.child.try_wait().ok().flatten();
        format!(
            "in-process kona consensus RPC for sequencer {index} did not become ready; \
             process_status={status:?}"
        )
    })?;

    info!(
        index,
        rpc_url = %rpc_url,
        auth_url = %auth_url,
        kona_rpc_url = %kona_rpc_url,
        p2p = %format!("127.0.0.1:{p2p_port}"),
        kona_p2p = %format!("127.0.0.1:{kona_p2p_port}"),
        metrics = %format!("127.0.0.1:{metrics_port}"),
        binary = %binary.display(),
        "native monomorphic world-chain client started (reth EL + in-process kona sequencer)"
    );

    Ok(SequencerService {
        id: format!("world-chain-el-{index}"),
        rpc_url,
        ws_url,
        auth_url,
        flashblocks_url: format!("ws://127.0.0.1:{ws_port}"),
        kona_rpc_url,
        kona_rpc_host_port: kona_rpc_port,
        p2p_host_port: p2p_port,
        kona_p2p_host_port: kona_p2p_port,
        metrics_target: MetricsTarget::new(
            format!("world-chain-el-{index}"),
            format!("host.docker.internal:{metrics_port}"),
        ),
        binary,
        _process: process,
    })
}

async fn connect_execution_peers(sequencers: &[SequencerService]) -> Result<()> {
    if sequencers.len() <= 1 {
        return Ok(());
    }

    let mut enodes = Vec::with_capacity(sequencers.len());
    for sequencer in sequencers {
        let info = wait_for_json_rpc(
            &sequencer.rpc_url,
            "admin_nodeInfo",
            json!([]),
            Duration::from_secs(30),
        )
        .await
        .wrap_err_with(|| format!("failed to read EL node info for {}", sequencer.id))?;
        let raw_enode = info
            .get("enode")
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("admin_nodeInfo for {} missing enode: {info}", sequencer.id))?;
        enodes.push(enode_with_host_port(
            raw_enode,
            "127.0.0.1",
            sequencer.p2p_host_port,
        )?);
    }

    for (source_index, source) in sequencers.iter().enumerate() {
        for (target_index, target) in sequencers.iter().enumerate() {
            if source_index == target_index {
                continue;
            }
            add_execution_peer(source, target, &enodes[target_index]).await?;
        }
    }

    let min_peers_per_node = u64::from(sequencers.len() > 1);
    let min_total_peer_connections = sequencers.len().saturating_sub(1) as u64 * 2;
    info!(
        nodes = sequencers.len(),
        min_peers_per_node, min_total_peer_connections, "waiting for world-chain EL peer graph"
    );
    let counts = retry_until(Duration::from_secs(120), Duration::from_millis(500), || async {
        redial_execution_peers(sequencers, &enodes).await;
        let counts = execution_peer_counts(sequencers).await?;
        let total: u64 = counts.iter().map(|(_, connected)| *connected).sum();
        let every_node_connected = counts
            .iter()
            .all(|(_, connected)| *connected >= min_peers_per_node);
        if every_node_connected && total >= min_total_peer_connections {
            Ok(counts)
        } else {
            bail!(
                "EL peer graph is not connected yet: {} (need every node >= {min_peers_per_node}, total >= {min_total_peer_connections})",
                peer_counts_summary(&counts)
            )
        }
    })
    .await
    .wrap_err("EL trusted peer graph did not form")?;

    info!(
        count = sequencers.len(),
        peer_counts = %peer_counts_summary(&counts),
        "world-chain EL trusted peer graph connected"
    );
    Ok(())
}

async fn redial_execution_peers(sequencers: &[SequencerService], enodes: &[String]) {
    for (source_index, source) in sequencers.iter().enumerate() {
        for (target_index, target) in sequencers.iter().enumerate() {
            if source_index == target_index {
                continue;
            }
            if let Err(err) = add_execution_peer_once(source, &enodes[target_index])
                .await
                .wrap_err_with(|| {
                    format!(
                        "failed to redial {} as trusted EL peer of {}",
                        target.id, source.id
                    )
                })
            {
                debug!(%err);
            }
        }
    }
}

async fn execution_peer_counts(sequencers: &[SequencerService]) -> Result<Vec<(String, u64)>> {
    let mut counts = Vec::with_capacity(sequencers.len());
    for sequencer in sequencers {
        let peer_count = json_rpc(&sequencer.rpc_url, "net_peerCount", json!([])).await?;
        counts.push((sequencer.id.clone(), json_rpc_quantity_to_u64(&peer_count)?));
    }
    Ok(counts)
}

fn peer_counts_summary(counts: &[(String, u64)]) -> String {
    counts
        .iter()
        .map(|(id, connected)| format!("{id}={connected}"))
        .collect::<Vec<_>>()
        .join(", ")
}

async fn add_execution_peer(
    source: &SequencerService,
    target: &SequencerService,
    enode: &str,
) -> Result<()> {
    retry_until(Duration::from_secs(30), Duration::from_millis(500), || {
        let enode = enode.to_string();
        async move { add_execution_peer_once(source, &enode).await }
    })
    .await
    .wrap_err_with(|| {
        format!(
            "failed to add {} as trusted and dialed EL peer of {}",
            target.id, source.id
        )
    })
}

async fn add_execution_peer_once(source: &SequencerService, enode: &str) -> Result<()> {
    let trusted = json_rpc(
        &source.rpc_url,
        "admin_addTrustedPeer",
        json!([enode.to_string()]),
    )
    .await?;
    if trusted.as_bool() == Some(false) {
        bail!("admin_addTrustedPeer returned false")
    }

    let added = json_rpc(&source.rpc_url, "admin_addPeer", json!([enode])).await?;
    if added.as_bool() == Some(false) {
        bail!("admin_addPeer returned false")
    }
    Ok(())
}

fn enode_with_host_port(enode: &str, host: &str, port: u16) -> Result<String> {
    let at = enode
        .rfind('@')
        .ok_or_else(|| eyre!("invalid enode without @: {enode}"))?;
    let endpoint = &enode[at + 1..];
    let query = endpoint
        .find('?')
        .map(|query_start| &endpoint[query_start..])
        .unwrap_or("");
    Ok(format!("{}{}:{}{}", &enode[..=at], host, port, query))
}

fn plan_conductor(index: usize, port_mode: DevnetPortMode) -> Result<ConductorPlan> {
    let consensus_host_port = match port_mode {
        DevnetPortMode::Stable => 50_050 + index as u16,
        DevnetPortMode::Dynamic => reserve_host_port()?,
    };
    let rpc_host_port = match port_mode {
        DevnetPortMode::Stable => 50_100 + index as u16,
        DevnetPortMode::Dynamic => reserve_host_port()?,
    };
    let ws_host_port = match port_mode {
        DevnetPortMode::Stable => 50_200 + index as u16,
        DevnetPortMode::Dynamic => reserve_host_port()?,
    };
    let metrics_host_port = match port_mode {
        DevnetPortMode::Stable => 50_300 + index as u16,
        DevnetPortMode::Dynamic => reserve_host_port()?,
    };
    let server_id = format!("sequencer-{}", index + 1);
    let consensus_advertised = format!("host.docker.internal:{consensus_host_port}");

    Ok(ConductorPlan {
        server_id,
        rpc_url: format!("http://127.0.0.1:{rpc_host_port}"),
        ws_url: format!("ws://127.0.0.1:{ws_host_port}"),
        consensus_advertised,
        rpc_host_port,
        ws_host_port,
        consensus_host_port,
        metrics_host_port,
        metrics_target: MetricsTarget::new(
            format!("op-conductor-{index}"),
            format!("host.docker.internal:{metrics_host_port}"),
        ),
    })
}

fn random_p2p_secret_key() -> String {
    loop {
        let bytes = rand::rng().random::<[u8; 32]>();
        if SecretKey::from_byte_array(&bytes).is_ok() {
            return hex::encode(bytes);
        }
    }
}

fn devnet_p2p_secret_key(secret_key_hex: &str) -> Result<SecretKey> {
    let bytes = hex::decode(secret_key_hex.trim_start_matches("0x"))
        .wrap_err("failed to decode devnet p2p private key")?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| eyre!("expected 32-byte p2p key, got {}", bytes.len()))?;
    SecretKey::from_byte_array(&bytes).wrap_err("invalid devnet p2p private key")
}

fn devnet_enode(secret_key_hex: &str, port: u16) -> Result<String> {
    let secret_key = devnet_p2p_secret_key(secret_key_hex)?;
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    Ok(NodeRecord::from_secret_key(addr, &secret_key).to_string())
}

fn devnet_trusted_peer(secret_key_hex: &str, host: &str, port: u16) -> Result<String> {
    let secret_key = devnet_p2p_secret_key(secret_key_hex)?;
    let host = Host::parse(host).wrap_err_with(|| format!("invalid devnet peer host {host}"))?;
    Ok(TrustedPeer::from_secret_key(host, port, &secret_key).to_string())
}

async fn start_conductor(
    index: usize,
    sequencer_count: usize,
    image: &ContainerImage,
    workdir: &Path,
    sequencer: &SequencerService,
    plan: &ConductorPlan,
) -> Result<ConductorService> {
    // op-conductor runs in a container and reaches the native in-process kona
    // node RPC via host.docker.internal on its directly-bound host port.
    let node_rpc = format!(
        "http://host.docker.internal:{}",
        sequencer.kona_rpc_host_port
    );
    let execution_rpc = host_internal_url(&sequencer.rpc_url)?;
    let min_peer_count = sequencer_count.saturating_sub(1).max(1).to_string();
    let mut cmd = vec![
        "--consensus.addr".to_string(),
        "0.0.0.0".to_string(),
        "--consensus.port".to_string(),
        CONDUCTOR_CONSENSUS_PORT.to_string(),
        "--consensus.advertised".to_string(),
        plan.consensus_advertised.clone(),
        "--raft.server.id".to_string(),
        plan.server_id.clone(),
        "--raft.storage.dir".to_string(),
        format!("/work/conductor-{index}"),
        "--rpc.addr".to_string(),
        "0.0.0.0".to_string(),
        "--rpc.port".to_string(),
        CONDUCTOR_RPC_PORT.to_string(),
        "--rpc.enable-admin".to_string(),
        "--rpc.enable-proxy".to_string(),
        "--websocket.server-port".to_string(),
        CONDUCTOR_WS_PORT.to_string(),
        "--node.rpc".to_string(),
        node_rpc,
        "--execution.rpc".to_string(),
        execution_rpc,
        "--rollup.config".to_string(),
        "/work/rollup.json".to_string(),
        "--healthcheck.interval".to_string(),
        CONDUCTOR_HEALTHCHECK_INTERVAL_SECS.to_string(),
        "--healthcheck.min-peer-count".to_string(),
        min_peer_count,
        "--healthcheck.unsafe-interval".to_string(),
        CONDUCTOR_HEALTHCHECK_UNSAFE_INTERVAL_SECS.to_string(),
        "--metrics.enabled".to_string(),
        "--metrics.addr".to_string(),
        "0.0.0.0".to_string(),
        "--metrics.port".to_string(),
        CONDUCTOR_METRICS_PORT.to_string(),
        "--log.format".to_string(),
        "logfmt".to_string(),
        "--log.level".to_string(),
        "DEBUG".to_string(),
    ];
    if index == 0 {
        cmd.push("--raft.bootstrap".to_string());
    }
    cmd.push("--paused".to_string());

    let container = GenericImage::new(image.repository.clone(), image.tag.clone())
        .with_entrypoint("op-conductor")
        .with_wait_for(WaitFor::seconds(3))
        .with_exposed_port(CONDUCTOR_RPC_PORT.tcp())
        .with_exposed_port(CONDUCTOR_WS_PORT.tcp())
        .with_exposed_port(CONDUCTOR_CONSENSUS_PORT.tcp())
        .with_exposed_port(CONDUCTOR_METRICS_PORT.tcp())
        .with_log_consumer(container_log_consumer(
            format!("op-conductor-{index}"),
            ProcessLogTarget::OpConductor,
        ))
        .with_cmd(cmd)
        .with_startup_timeout(Duration::from_secs(90))
        .with_mount(Mount::bind_mount(
            workdir.to_string_lossy().to_string(),
            "/work",
        ))
        .with_mapped_port(plan.rpc_host_port, CONDUCTOR_RPC_PORT.tcp())
        .with_mapped_port(plan.ws_host_port, CONDUCTOR_WS_PORT.tcp())
        .with_mapped_port(plan.consensus_host_port, CONDUCTOR_CONSENSUS_PORT.tcp())
        .with_mapped_port(plan.metrics_host_port, CONDUCTOR_METRICS_PORT.tcp())
        .start()
        .await
        .wrap_err_with(|| format!("failed to start op-conductor {index}"))?;

    let service = ConductorService {
        id: format!("op-conductor-{index}"),
        server_id: plan.server_id.clone(),
        rpc_url: plan.rpc_url.clone(),
        ws_url: plan.ws_url.clone(),
        consensus_advertised: plan.consensus_advertised.clone(),
        metrics_target: plan.metrics_target.clone(),
        image: image.clone(),
        _container: container,
    };

    info!(
        id = %service.id,
        rpc_url = %service.rpc_url,
        consensus_advertised = %service.consensus_advertised,
        sequencer_count,
        "op-conductor started"
    );
    Ok(service)
}

async fn wait_for_kona_consensus_mesh(sequencers: &[SequencerService]) -> Result<()> {
    if sequencers.len() <= 1 {
        return Ok(());
    }

    let expected = sequencers.len().saturating_sub(1) as u64;
    info!(
        nodes = sequencers.len(),
        expected_peers_per_node = expected,
        "waiting for in-process kona consensus P2P mesh"
    );
    for sequencer in sequencers {
        retry_until(
            Duration::from_secs(60),
            Duration::from_millis(500),
            || async {
                let peers = json_rpc(&sequencer.kona_rpc_url, "opp2p_peers", json!([true])).await?;
                let connected = peers
                    .get("totalConnected")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        eyre!(
                            "opp2p_peers for {} missing totalConnected: {peers}",
                            sequencer.id
                        )
                    })?;
                if connected >= expected {
                    Ok(())
                } else {
                    bail!(
                        "{} has {connected} connected kona consensus peers, expected {expected}",
                        sequencer.id
                    )
                }
            },
        )
        .await
        .wrap_err_with(|| format!("kona consensus P2P mesh did not form for {}", sequencer.id))?;
    }

    info!(
        count = sequencers.len(),
        "in-process kona consensus P2P mesh connected"
    );
    Ok(())
}

async fn configure_conductor_cluster(conductors: &[ConductorService]) -> Result<()> {
    if conductors.is_empty() {
        return Ok(());
    }

    let bootstrap = &conductors[0];
    wait_for_conductor_leader(bootstrap, Duration::from_secs(90)).await?;

    for conductor in conductors.iter().skip(1) {
        retry_until(
            Duration::from_secs(20),
            Duration::from_millis(500),
            || async {
                json_rpc(
                    &bootstrap.rpc_url,
                    "conductor_addServerAsNonvoter",
                    json!([conductor.server_id, conductor.consensus_advertised, 0]),
                )
                .await
                .map(|_| ())
            },
        )
        .await
        .wrap_err_with(|| format!("failed to add {} as non-voter", conductor.id))?;

        retry_until(
            Duration::from_secs(20),
            Duration::from_millis(500),
            || async {
                json_rpc(
                    &bootstrap.rpc_url,
                    "conductor_addServerAsVoter",
                    json!([conductor.server_id, conductor.consensus_advertised, 0]),
                )
                .await
                .map(|_| ())
            },
        )
        .await
        .wrap_err_with(|| format!("failed to add {} as voter", conductor.id))?;
    }

    for conductor in conductors {
        retry_until(
            Duration::from_secs(20),
            Duration::from_millis(500),
            || async {
                json_rpc(&conductor.rpc_url, "conductor_resume", json!([]))
                    .await
                    .map(|_| ())
            },
        )
        .await
        .wrap_err_with(|| format!("failed to resume {}", conductor.id))?;
    }

    info!(
        count = conductors.len(),
        "op-conductor raft cluster configured"
    );
    Ok(())
}

async fn wait_for_conductor_health(conductors: &[ConductorService]) -> Result<()> {
    for conductor in conductors {
        retry_until(
            Duration::from_secs(90),
            Duration::from_millis(500),
            || async {
                let healthy =
                    conductor_bool(&conductor.rpc_url, "conductor_sequencerHealthy").await?;
                if healthy {
                    Ok(())
                } else {
                    bail!("{} sequencer is not healthy yet", conductor.id)
                }
            },
        )
        .await
        .wrap_err_with(|| format!("{} never became sequencer-healthy", conductor.id))?;
    }

    info!(
        count = conductors.len(),
        "op-conductor sequencer health checks passed"
    );
    Ok(())
}

async fn wait_for_conductor_leader(bootstrap: &ConductorService, timeout: Duration) -> Result<()> {
    if let Err(err) = retry_until(timeout, Duration::from_millis(500), || async {
        let leader = conductor_bool(&bootstrap.rpc_url, "conductor_leader").await?;
        if leader {
            Ok(())
        } else {
            bail!("bootstrap conductor is not raft leader yet")
        }
    })
    .await
    {
        let logs = container_logs(&bootstrap._container).await;
        return Err(err).wrap_err_with(|| {
            format!(
                "bootstrap conductor never became raft leader; container logs:\n{}",
                logs
            )
        });
    }

    Ok(())
}

async fn start_bootstrap_sequencer(sequencer: &SequencerService) -> Result<()> {
    let kona_rpc_url = &sequencer.kona_rpc_url;
    retry_until(
        Duration::from_secs(30),
        Duration::from_millis(500),
        || async {
            let active = json_rpc(kona_rpc_url, "admin_sequencerActive", json!([]))
                .await?
                .as_bool()
                .ok_or_else(|| eyre!("admin_sequencerActive did not return a bool"))?;
            if active {
                return Ok(());
            }

            let sync_status = json_rpc(kona_rpc_url, "optimism_syncStatus", json!([])).await?;
            let unsafe_hash = sync_status
                .pointer("/unsafe_l2/hash")
                .and_then(Value::as_str)
                .ok_or_else(|| eyre!("optimism_syncStatus missing unsafe_l2.hash: {sync_status}"))?
                .to_string();
            json_rpc(kona_rpc_url, "admin_startSequencer", json!([unsafe_hash]))
                .await
                .map(|_| ())
        },
    )
    .await
    .wrap_err_with(|| {
        format!(
            "failed to explicitly start in-process kona bootstrap sequencer {}",
            sequencer.id
        )
    })?;

    info!(
        sequencer = %sequencer.id,
        kona_rpc_url = %sequencer.kona_rpc_url,
        "bootstrap in-process kona sequencer started"
    );
    Ok(())
}

async fn start_batcher(
    image: &ContainerImage,
    l1_rpc: &str,
    conductor_rpc: &str,
) -> Result<ContainerService> {
    let cmd = vec![
        "--l1-eth-rpc".to_string(),
        l1_rpc.to_string(),
        "--l2-eth-rpc".to_string(),
        conductor_rpc.to_string(),
        "--rollup-rpc".to_string(),
        conductor_rpc.to_string(),
        "--private-key".to_string(),
        BATCHER_PRIVATE_KEY.to_string(),
        "--data-availability-type".to_string(),
        "calldata".to_string(),
        "--poll-interval".to_string(),
        "1s".to_string(),
        "--max-channel-duration".to_string(),
        OP_BATCHER_MAX_CHANNEL_DURATION_L1_BLOCKS.to_string(),
        "--sub-safety-margin".to_string(),
        "0".to_string(),
        "--num-confirmations".to_string(),
        "1".to_string(),
        "--network-timeout".to_string(),
        OP_TXMGR_NETWORK_TIMEOUT.to_string(),
        "--resubmission-timeout".to_string(),
        OP_TXMGR_RESUBMISSION_TIMEOUT.to_string(),
        "--rpc.addr".to_string(),
        "0.0.0.0".to_string(),
        "--rpc.port".to_string(),
        SERVICE_RPC_PORT.to_string(),
        "--rpc.enable-admin".to_string(),
        "--metrics.enabled".to_string(),
        "--metrics.addr".to_string(),
        "0.0.0.0".to_string(),
        "--metrics.port".to_string(),
        SERVICE_METRICS_PORT.to_string(),
        "--log.format".to_string(),
        "logfmt".to_string(),
        "--log.level".to_string(),
        "DEBUG".to_string(),
    ];
    start_aux_service(
        "op-batcher",
        DevnetComponentKind::OpBatcher,
        image,
        cmd,
        None,
    )
    .await
}

async fn start_proposer(
    image: &ContainerImage,
    l1_rpc: &str,
    rollup_rpc: &str,
    game_factory: &str,
) -> Result<ContainerService> {
    let cmd = vec![
        "--l1-eth-rpc".to_string(),
        l1_rpc.to_string(),
        "--rollup-rpc".to_string(),
        rollup_rpc.to_string(),
        "--game-factory-address".to_string(),
        game_factory.to_string(),
        "--game-type".to_string(),
        OP_PROPOSER_PERMISSIONED_GAME_TYPE.to_string(),
        "--private-key".to_string(),
        PROPOSER_PRIVATE_KEY.to_string(),
        "--proposal-interval".to_string(),
        "6s".to_string(),
        "--poll-interval".to_string(),
        "1s".to_string(),
        "--allow-non-finalized".to_string(),
        "--num-confirmations".to_string(),
        "1".to_string(),
        "--network-timeout".to_string(),
        OP_TXMGR_NETWORK_TIMEOUT.to_string(),
        "--resubmission-timeout".to_string(),
        OP_TXMGR_RESUBMISSION_TIMEOUT.to_string(),
        "--rpc.addr".to_string(),
        "0.0.0.0".to_string(),
        "--rpc.port".to_string(),
        SERVICE_RPC_PORT.to_string(),
        "--rpc.enable-admin".to_string(),
        "--metrics.enabled".to_string(),
        "--metrics.addr".to_string(),
        "0.0.0.0".to_string(),
        "--metrics.port".to_string(),
        SERVICE_METRICS_PORT.to_string(),
        "--log.format".to_string(),
        "logfmt".to_string(),
        "--log.level".to_string(),
        "DEBUG".to_string(),
    ];
    start_aux_service(
        "op-proposer",
        DevnetComponentKind::OpProposer,
        image,
        cmd,
        None,
    )
    .await
}

async fn start_challenger(
    image: &ContainerImage,
    workdir: &Path,
    l1_rpc: &str,
    l2_rpc: &str,
    rollup_rpc: &str,
    game_factory: &str,
) -> Result<ContainerService> {
    let cmd = vec![
        "--l1-eth-rpc".to_string(),
        l1_rpc.to_string(),
        "--l1-rpc-kind".to_string(),
        "basic".to_string(),
        "--l1-beacon".to_string(),
        l1_rpc.to_string(),
        "--l2-eth-rpc".to_string(),
        l2_rpc.to_string(),
        "--rollup-rpc".to_string(),
        rollup_rpc.to_string(),
        "--game-factory-address".to_string(),
        game_factory.to_string(),
        "--game-types".to_string(),
        "permissioned".to_string(),
        "--prestates-url".to_string(),
        "file:///work/prestates".to_string(),
        "--private-key".to_string(),
        CHALLENGER_PRIVATE_KEY.to_string(),
        "--datadir".to_string(),
        "/work/op-challenger".to_string(),
        "--l1-genesis".to_string(),
        "/work/l1-genesis.json".to_string(),
        "--l2-genesis".to_string(),
        "/work/genesis.json".to_string(),
        "--rollup-config".to_string(),
        "/work/rollup.json".to_string(),
        "--metrics.enabled".to_string(),
        "--metrics.addr".to_string(),
        "0.0.0.0".to_string(),
        "--metrics.port".to_string(),
        SERVICE_METRICS_PORT.to_string(),
        "--log.format".to_string(),
        "logfmt".to_string(),
        "--log.level".to_string(),
        "DEBUG".to_string(),
    ];
    start_aux_service(
        "op-challenger",
        DevnetComponentKind::OpChallenger,
        image,
        cmd,
        Some(workdir),
    )
    .await
}

async fn start_aux_service(
    id: &str,
    kind: DevnetComponentKind,
    image: &ContainerImage,
    cmd: Vec<String>,
    mount: Option<&Path>,
) -> Result<ContainerService> {
    info!(
        id,
        image = %image.reference(),
        command = %format!("{id} {}", cmd.join(" ")),
        "starting OP Stack devnet service"
    );

    let mut request = GenericImage::new(image.repository.clone(), image.tag.clone())
        .with_entrypoint(id)
        .with_wait_for(WaitFor::seconds(3))
        .with_exposed_port(SERVICE_RPC_PORT.tcp())
        .with_exposed_port(SERVICE_METRICS_PORT.tcp())
        .with_log_consumer(container_log_consumer(
            id.to_string(),
            service_log_target(id),
        ))
        .with_cmd(cmd)
        .with_startup_timeout(Duration::from_secs(90));

    if let Some(mount) = mount {
        request = request.with_mount(Mount::bind_mount(
            mount.to_string_lossy().to_string(),
            "/work",
        ));
    }

    let container = request
        .start()
        .await
        .wrap_err_with(|| format!("failed to start {id}"))?;
    let host = container.get_host().await?;
    let rpc_port = container
        .get_host_port_ipv4(SERVICE_RPC_PORT.tcp())
        .await
        .ok();
    let metrics_port = container
        .get_host_port_ipv4(SERVICE_METRICS_PORT.tcp())
        .await
        .ok();
    wait_for_aux_service(id, &container, &host.to_string(), metrics_port).await?;
    info!(
        id,
        rpc_url = rpc_port.map(|port| format!("http://{host}:{port}")),
        metrics_url = metrics_port.map(|port| format!("http://{host}:{port}/metrics")),
        "OP Stack devnet service is ready"
    );
    Ok(ContainerService {
        id: id.to_string(),
        kind,
        rpc_url: rpc_port.map(|port| format!("http://{host}:{port}")),
        metrics_target: metrics_port
            .map(|port| MetricsTarget::new(id.to_string(), format!("host.docker.internal:{port}"))),
        image: image.clone(),
        _container: container,
    })
}

async fn wait_for_aux_service(
    id: &str,
    container: &ContainerAsync<GenericImage>,
    host: &str,
    metrics_port: Option<u16>,
) -> Result<()> {
    let Some(metrics_port) = metrics_port else {
        return Ok(());
    };

    let url = format!("http://{host}:{metrics_port}/metrics");
    if let Err(err) = retry_until(Duration::from_secs(10), Duration::from_millis(500), || {
        let url = url.clone();
        async move { require_http_success(&url).await }
    })
    .await
    {
        let logs = tail_text(&container_logs(container).await, 120);
        return Err(err).wrap_err_with(|| {
            format!("{id} did not expose a healthy metrics endpoint at {url}; logs:\n{logs}")
        });
    }

    for _ in 0..3 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Err(err) = require_http_success(&url).await {
            let logs = tail_text(&container_logs(container).await, 120);
            return Err(err).wrap_err_with(|| {
                format!("{id} exited or stopped serving metrics after initial readiness at {url}; logs:\n{logs}")
            });
        }
    }

    Ok(())
}

async fn require_http_success(url: &str) -> Result<()> {
    let response = reqwest::get(url)
        .await
        .wrap_err_with(|| format!("failed to query metrics endpoint {url}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        bail!("metrics endpoint {url} returned {}", response.status())
    }
}

async fn container_logs(container: &ContainerAsync<GenericImage>) -> String {
    let stdout = container.stdout_to_vec().await.unwrap_or_default();
    let stderr = container.stderr_to_vec().await.unwrap_or_default();
    let stdout = String::from_utf8_lossy(&stdout);
    let stderr = String::from_utf8_lossy(&stderr);
    format!("stdout:\n{stdout}\nstderr:\n{stderr}")
}

async fn run_native_command(label: &str, binary: &Path, args: &[String]) -> Result<()> {
    info!(
        label,
        command = %format!("{} {}", binary.display(), args.join(" ")),
        "running devnet native command"
    );
    let output = Command::new(binary)
        .args(args)
        .output()
        .await
        .wrap_err_with(|| format!("failed to spawn native command for {label}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.trim().is_empty() {
        emit_command_logs(label, ProcessLogTarget::WorldChainEl, &stdout);
    }
    if !stderr.trim().is_empty() {
        emit_command_logs(label, ProcessLogTarget::WorldChainEl, &stderr);
    }
    if !output.status.success() {
        bail!(
            "native command failed for {label} with status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            stdout,
            stderr
        );
    }
    Ok(())
}

fn emit_command_logs(label: &str, target: ProcessLogTarget, output: &str) {
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        emit_process_log(target, label, line);
    }
}

fn spawn_native_process(id: &str, binary: &Path, args: &[String]) -> Result<NativeProcess> {
    info!(
        id,
        command = %format!("{} {}", binary.display(), args.join(" ")),
        "starting devnet native process"
    );

    let mut child = Command::new(binary)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .wrap_err_with(|| format!("failed to spawn native process {id}"))?;

    let mut log_tasks = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        log_tasks.push(tokio::spawn(log_process_stream(
            id.to_string(),
            ProcessLogTarget::WorldChainEl,
            "stdout",
            stdout,
        )));
    }
    if let Some(stderr) = child.stderr.take() {
        log_tasks.push(tokio::spawn(log_process_stream(
            id.to_string(),
            ProcessLogTarget::WorldChainEl,
            "stderr",
            stderr,
        )));
    }

    Ok(NativeProcess {
        id: id.to_string(),
        child,
        _log_tasks: log_tasks,
    })
}

async fn log_process_stream<R>(
    id: String,
    target: ProcessLogTarget,
    stream_name: &'static str,
    stream: R,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut lines = BufReader::new(stream).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                emit_process_log(target, &id, &line);
            }
            Ok(None) => break,
            Err(err) => {
                debug!(
                    process = %id,
                    %err,
                    "failed to read {stream_name} from native devnet process"
                );
                break;
            }
        }
    }
}

fn service_log_target(id: &str) -> ProcessLogTarget {
    match id {
        "op-batcher" => ProcessLogTarget::OpBatcher,
        "op-proposer" => ProcessLogTarget::OpProposer,
        "op-challenger" => ProcessLogTarget::OpChallenger,
        _ => ProcessLogTarget::OpStackService,
    }
}

fn build_components(
    config: &HaSequencerConfig,
    l1_rpc_url: &str,
    sequencers: &[SequencerService],
    conductors: &[ConductorService],
    batcher: Option<&ContainerService>,
    proposer: Option<&ContainerService>,
    challenger: Option<&ContainerService>,
    observability: Option<&ObservabilityStack>,
    game_factory: &str,
) -> Vec<DevnetComponent> {
    let mut components = vec![
        DevnetComponent::new(
            "l1-dev-chain",
            DevnetComponentKind::L1DevChain,
            DevnetComponentStatus::Running,
        )
        .with_endpoint("rpc", l1_rpc_url.to_string())
        .with_note("Anvil L1 initialized from op-deployer genesis target with OP contracts in state"),
        DevnetComponent::new(
            "op-contract-deployer",
            DevnetComponentKind::OpContractDeployer,
            DevnetComponentStatus::Running,
        )
        .with_image(config.images.op_deployer.clone())
        .with_note("op-deployer generated L1 contract state, L2 genesis, rollup config, and L1 address outputs"),
    ];

    for service in sequencers {
        components.push(
            DevnetComponent::new(
                service.id.clone(),
                DevnetComponentKind::WorldChainExecutionNode,
                DevnetComponentStatus::Running,
            )
            .with_endpoint("rpc", service.rpc_url.clone())
            .with_endpoint("ws", service.ws_url.clone())
            .with_endpoint("engine", service.auth_url.clone())
            .with_endpoint("kona-rpc", service.kona_rpc_url.clone())
            .with_endpoint("p2p", format!("127.0.0.1:{}", service.p2p_host_port))
            .with_endpoint(
                "kona-p2p",
                format!("127.0.0.1:{}", service.kona_p2p_host_port),
            )
            .with_note(
                "monomorphic World Chain client: native reth EL + in-process kona consensus/sequencer, flashblocks enabled and trusted EL peers",
            )
            .with_note(format!(
                "PBH disabled with zero reserved blockspace and sentinel entrypoint {PBH_DISABLED_ENTRYPOINT}"
            ))
            .with_note(format!("binary={}", service.binary.display())),
        );
        components.push(
            DevnetComponent::new(
                format!(
                    "flashblocks-{}",
                    service.id.trim_start_matches("world-chain-el-")
                ),
                DevnetComponentKind::Flashblocks,
                DevnetComponentStatus::Running,
            )
            .with_endpoint("ws", service.flashblocks_url.clone())
            .with_note("flashblocks enabled by default; rollup-boost is intentionally absent"),
        );
    }

    for service in conductors {
        components.push(
            DevnetComponent::new(
                service.id.clone(),
                DevnetComponentKind::OpConductor,
                DevnetComponentStatus::Running,
            )
            .with_image(service.image.clone())
            .with_endpoint("rpc", service.rpc_url.clone())
            .with_endpoint("ws", service.ws_url.clone())
            .with_endpoint("raft", service.consensus_advertised.clone())
            .with_note("member of the local op-conductor raft cluster"),
        );
    }

    for service in [batcher, proposer, challenger].into_iter().flatten() {
        let mut component = DevnetComponent::new(
            service.id.clone(),
            service.kind,
            DevnetComponentStatus::Running,
        )
        .with_image(service.image.clone());
        if let Some(url) = &service.rpc_url {
            component = component.with_endpoint("rpc", url.clone());
        }
        if service.kind == DevnetComponentKind::OpProposer
            || service.kind == DevnetComponentKind::OpChallenger
        {
            component = component.with_note(format!("DisputeGameFactoryProxy={game_factory}"));
        }
        if service.kind == DevnetComponentKind::OpChallenger {
            component = component.with_note(
                "uses a lifecycle-owned local prestates directory; generated Cannon prestates are a remaining parity gap for exercising live dispute games",
            );
        }
        components.push(component);
    }

    components.push(
        DevnetComponent::new(
            "world-contracts-deployer",
            DevnetComponentKind::WorldContractsDeployer,
            DevnetComponentStatus::Deferred,
        )
        .with_note("not run by the native devnet; FeeEscrow and FeeRecipient are intentionally not deployed")
        .with_note("PBH contracts are deprecated and intentionally omitted"),
    );

    if let Some(observability) = observability {
        components.extend(observability.components());
    }

    components
}

async fn run_docker(label: &str, args: Vec<String>) -> Result<()> {
    info!(label, command = %format!("docker {}", args.join(" ")), "running devnet docker command");
    let output = Command::new("docker")
        .args(&args)
        .output()
        .await
        .wrap_err_with(|| format!("failed to spawn docker command for {label}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.trim().is_empty() {
        emit_command_logs(label, ProcessLogTarget::OpDeployer, &stdout);
    }
    if !stderr.trim().is_empty() {
        emit_command_logs(label, ProcessLogTarget::OpDeployer, &stderr);
    }
    if !output.status.success() {
        bail!(
            "docker command failed for {label} with status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            stdout,
            stderr
        );
    }
    Ok(())
}

async fn wait_for_l2_blocks(rpc_url: &str, min_block: u64, timeout: Duration) -> Result<()> {
    retry_until(timeout, Duration::from_millis(500), || async {
        let provider = ProviderBuilder::new().connect_http(Url::parse(rpc_url)?);
        let block = provider.get_block_number().await?;
        if block >= min_block {
            Ok(())
        } else {
            bail!("latest L2 block {block} is below expected {min_block}")
        }
    })
    .await
}

async fn wait_for_l2_blocks_with_logs(
    rpc_url: &str,
    min_block: u64,
    timeout: Duration,
    sequencers: &[SequencerService],
    conductors: &[ConductorService],
) -> Result<()> {
    if let Err(err) = wait_for_l2_blocks(rpc_url, min_block, timeout).await {
        let mut diagnostics = String::new();
        diagnostics.push_str("in-process kona consensus status:\n");
        for sequencer in sequencers {
            let sync_status = json_rpc(&sequencer.kona_rpc_url, "optimism_syncStatus", json!([]))
                .await
                .map(|value| value.to_string())
                .unwrap_or_else(|err| format!("error: {err}"));
            let sequencer_active =
                json_rpc(&sequencer.kona_rpc_url, "admin_sequencerActive", json!([]))
                    .await
                    .map(|value| value.to_string())
                    .unwrap_or_else(|err| format!("error: {err}"));
            diagnostics.push_str(&format!(
                "\n{} kona_rpc={} sequencer_active={} sync_status={}\n",
                sequencer.id, sequencer.kona_rpc_url, sequencer_active, sync_status
            ));
        }

        diagnostics.push_str("\nop-conductor status:\n");
        for conductor in conductors {
            let leader = json_rpc(&conductor.rpc_url, "conductor_leader", json!([]))
                .await
                .map(|value| value.to_string())
                .unwrap_or_else(|err| format!("error: {err}"));
            let active = json_rpc(&conductor.rpc_url, "conductor_active", json!([]))
                .await
                .map(|value| value.to_string())
                .unwrap_or_else(|err| format!("error: {err}"));
            let healthy = json_rpc(&conductor.rpc_url, "conductor_sequencerHealthy", json!([]))
                .await
                .map(|value| value.to_string())
                .unwrap_or_else(|err| format!("error: {err}"));
            let logs = tail_text(&container_logs(&conductor._container).await, 160);
            diagnostics.push_str(&format!(
                "\n{} rpc={} leader={} active={} sequencer_healthy={}\nlogs:\n{}\n",
                conductor.id, conductor.rpc_url, leader, active, healthy, logs
            ));
        }

        return Err(err).wrap_err_with(|| {
            format!(
                "timed out waiting for L2 block {min_block} on {rpc_url}; diagnostics:\n{diagnostics}"
            )
        });
    }

    Ok(())
}

fn tail_text(text: &str, max_lines: usize) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

async fn wait_for_rpc_chain_id(rpc_url: &str, timeout: Duration) -> Result<()> {
    retry_until(timeout, Duration::from_millis(500), || async {
        let provider = ProviderBuilder::new().connect_http(Url::parse(rpc_url)?);
        let chain_id = provider.get_chain_id().await?;
        if chain_id == DEV_CHAIN_ID {
            Ok(())
        } else {
            bail!("expected chain id {DEV_CHAIN_ID}, got {chain_id}")
        }
    })
    .await
}

async fn block_hash(rpc_url: &str, number: u64) -> Result<String> {
    let provider = ProviderBuilder::new().connect_http(Url::parse(rpc_url)?);
    let block = provider
        .get_block_by_number(BlockNumberOrTag::Number(number))
        .await?
        .ok_or_else(|| eyre!("block {number} missing from {rpc_url}"))?;
    Ok(format!("{:#x}", block.header.hash))
}

async fn wait_for_json_rpc(
    rpc_url: &str,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    retry_until(timeout, Duration::from_millis(500), || {
        let params = params.clone();
        async move { json_rpc(rpc_url, method, params).await }
    })
    .await
}

async fn conductor_bool(rpc_url: &str, method: &str) -> Result<bool> {
    json_rpc(rpc_url, method, json!([]))
        .await?
        .as_bool()
        .ok_or_else(|| eyre!("{method} did not return a bool"))
}

async fn json_rpc(rpc_url: &str, method: &str, params: Value) -> Result<Value> {
    let client = reqwest::Client::new();
    let response = client
        .post(rpc_url)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        }))
        .send()
        .await
        .wrap_err_with(|| format!("failed to send {method} to {rpc_url}"))?;
    let value: Value = response
        .json()
        .await
        .wrap_err_with(|| format!("invalid JSON-RPC response for {method} from {rpc_url}"))?;
    if let Some(error) = value.get("error") {
        bail!("JSON-RPC {method} failed at {rpc_url}: {error}");
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| eyre!("JSON-RPC {method} response missing result from {rpc_url}: {value}"))
}

fn json_rpc_quantity_to_u64(value: &Value) -> Result<u64> {
    if let Some(number) = value.as_u64() {
        return Ok(number);
    }
    let quantity = value
        .as_str()
        .ok_or_else(|| eyre!("JSON-RPC quantity is not a string or number: {value}"))?;
    let digits = quantity.strip_prefix("0x").unwrap_or(quantity);
    if digits.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(digits, 16)
        .wrap_err_with(|| format!("invalid JSON-RPC hex quantity {quantity}"))
}

async fn retry_until<F, Fut, T>(timeout: Duration, interval: Duration, mut op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let started = Instant::now();
    let mut last_error = None;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                if started.elapsed() >= timeout {
                    return Err(err).wrap_err_with(|| {
                        if let Some(last) = last_error {
                            format!("timed out after {timeout:?}; previous error: {last}")
                        } else {
                            format!("timed out after {timeout:?}")
                        }
                    });
                }
                last_error = Some(err.to_string());
                tokio::time::sleep(interval).await;
            }
        }
    }
}

fn host_internal_url(public_url: &str) -> Result<String> {
    let url = Url::parse(public_url)?;
    let port = url
        .port()
        .ok_or_else(|| eyre!("URL {public_url} has no port"))?;
    Ok(format!("{}://host.docker.internal:{port}", url.scheme()))
}

fn l1_address(addresses: &Value, name: &str) -> Result<String> {
    addresses
        .get(name)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| eyre!("op-deployer l1-addresses.json missing {name}"))
}

fn reserve_host_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

fn world_chain_binary() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("WORLD_CHAIN_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        bail!(
            "WORLD_CHAIN_BIN points to {}, but that file does not exist",
            path.display()
        );
    }

    let bin_name = if cfg!(windows) {
        "world-chain.exe"
    } else {
        "world-chain"
    };
    let current_exe =
        std::env::current_exe().wrap_err("failed to locate current executable path")?;
    let mut candidates = Vec::new();
    if let Some(parent) = current_exe.parent() {
        candidates.push(parent.join(bin_name));
        if parent.file_name().and_then(|name| name.to_str()) == Some("deps")
            && let Some(target_profile_dir) = parent.parent()
        {
            candidates.push(target_profile_dir.join(bin_name));
        }
    }
    if let Ok(target_dir) = std::env::var("CARGO_TARGET_DIR") {
        let target_dir = PathBuf::from(target_dir);
        candidates.push(target_dir.join("debug").join(bin_name));
        candidates.push(target_dir.join("release").join(bin_name));
    }
    let repo_root = repo_root()?;
    candidates.push(repo_root.join("target/debug").join(bin_name));
    candidates.push(repo_root.join("target/release").join(bin_name));

    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    bail!(
        "failed to find native world-chain binary; run `cargo build -p world-chain` or set WORLD_CHAIN_BIN"
    )
}

fn repo_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| eyre!("failed to derive workspace root from CARGO_MANIFEST_DIR"))
}

fn read_json(path: &Path) -> Result<Value> {
    let bytes = fs::read(path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
    serde_json::from_slice(&bytes).wrap_err_with(|| format!("invalid JSON in {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_enode_endpoint_for_host_reachable_peer() {
        let enode = "enode://abcdef@0.0.0.0:30303?discport=0";

        let rewritten = enode_with_host_port(enode, "127.0.0.1", 31000).unwrap();

        assert_eq!(rewritten, "enode://abcdef@127.0.0.1:31000?discport=0");
    }

    #[test]
    fn parses_json_rpc_peer_count_quantities() {
        assert_eq!(json_rpc_quantity_to_u64(&json!("0x0")).unwrap(), 0);
        assert_eq!(json_rpc_quantity_to_u64(&json!("0x2")).unwrap(), 2);
        assert_eq!(json_rpc_quantity_to_u64(&json!(3)).unwrap(), 3);
    }

    #[test]
    fn derives_deterministic_devnet_enode() {
        let enode = devnet_enode(&format!("{:064x}", 10_000), 30_303).unwrap();

        assert_eq!(
            enode,
            "enode://7a36d7efeac579690f7b89c8982329303a02bd710bc87f4eaaf5cfd84c2f6faecdeb2ea308a7e64028781419882b4619644b637acc3ea59824452172e52e24f9@127.0.0.1:30303"
        );
    }

    #[test]
    fn patches_jovian_genesis_extra_data() {
        let mut genesis = json!({ "extraData": "0x00" });
        let rollup = json!({
            "chain_op_config": {
                "eip1559DenominatorCanyon": 250,
                "eip1559Elasticity": 10
            }
        });
        let hardforks = WorldChainHardforkConfig::through(WorldChainHardfork::Jovian);

        patch_l2_genesis_base_fee_extra_data(&mut genesis, rollup.as_object().unwrap(), &hardforks)
            .unwrap();

        assert_eq!(genesis["extraData"], "0x01000000fa0000000a0000000000000000");
    }

    #[test]
    fn renders_op_deployer_intent_with_tagged_contract_locators() {
        let intent = render_intent(&HaSequencerConfig::default());

        assert!(intent.contains("l1ContractsLocator = \"tag://op-contracts/v3.0.0-rc.2\""));
        assert!(intent.contains("l2ContractsLocator = \"tag://op-contracts/v3.0.0-rc.2\""));
        assert!(!intent.contains("https://storage.googleapis.com/oplabs-contract-artifacts"));
    }
}
