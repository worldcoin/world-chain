use std::{
    fs,
    io::Read,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_eips::{BlockNumberOrTag, eip1559::BaseFeeParams};
use alloy_genesis::Genesis;
use alloy_network::EthereumWallet;
use alloy_primitives::{Address, B64, U256, hex, keccak256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_signer_local::PrivateKeySigner;
use base64::prelude::{BASE64_STANDARD, Engine};
use eyre::eyre::{Context, Result, bail, eyre};
use flate2::read::GzDecoder;
use futures::future::try_join_all;
use jsonrpsee::server::ServerHandle;
use op_alloy_consensus::{encode_holocene_extra_data, encode_jovian_extra_data};
use rand::Rng as _;
use reth_chainspec::EthChainSpec;
use reth_network_peers::{NodeRecord, TrustedPeer};
use secp256k1::SecretKey;
use serde::Deserialize;
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
use tracing::{Instrument, debug, info, info_span, warn};
use url::{Host, Url};
use world_chain_chainspec::{WorldChainHardfork, WorldChainSpec};
use world_chain_challenger::{AlloyChallengerClient, ChallengerConfig, WorldChainChallenger};
use world_chain_defender::{AlloyDefenderClient, DefenderConfig, WorldChainDefender};
use world_chain_proof_succinct_host_utils::{
    env_prover::{EnvSuccinctProver, SP1ProofMode, Sp1ProverKind},
    online::OnlineHostConfig,
};
use world_chain_proof_worker::{ProofWorker, ProofWorkerConfig};
use world_chain_proofs::{OptimismConsensusClient, PROOF_SYSTEM_VERSION, PROOF_THRESHOLD};
use world_chain_proposer::{AlloyProofSystemClient, ProposerConfig, WorldChainProposer};
use world_chain_prover_service::{
    ProverService, ProverServiceConfig, RpcProverServiceClient, start_rpc_server,
};
use world_chain_sp1_worker::{Sp1Backend, Sp1BackendConfig};
use world_chain_test_utils::DEV_CHAIN_ID;

use crate::{
    DevnetComponent, DevnetComponentKind, DevnetComponentStatus, DevnetPortMode, L1Stack,
    L1StackConfig, MetricsTarget, ObservabilityStack, WorldChainHardforkConfig,
    component::ContainerImage,
    op_stack::{HaSequencerConfig, HaSequencerTopology},
    process_logs::{ProcessLogTarget, container_log_consumer, emit_process_log},
};

const ANVIL_RPC_PORT: u16 = 8545;
const OP_NODE_RPC_PORT: u16 = 9545;
const OP_NODE_METRICS_PORT: u16 = 7300;
const OP_NODE_P2P_PORT: u16 = 9222;
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
// Single-block proving range: witness cost is range_length × O(tip − target), so a 1-block
// range cuts witness collection ~10× vs the prior 10. Mirrors Base's e2e proving one block.
const PROOF_SYSTEM_BLOCK_INTERVAL: u64 = 1;
const PROOF_SYSTEM_INTERMEDIATE_BLOCK_INTERVAL: u64 = 1;
/// Poll interval for the in-process SP1 worker leasing jobs from the prover-service.
const SP1_WORKER_POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Env var enabling the in-process defender, prover-service, and SP1 worker. Off by default:
/// real proving needs the SP1 ELFs. Set a prover backend (`cpu`/`mock`/`network`) to turn it on.
const SP1_WORKER_PROVER_ENV: &str = "DEVNET_SP1_WORKER_PROVER";
/// Bond, in wei, sent with every `WorldChainProofSystemFactory.propose`.
/// Matches `PROPOSER_BOND` (1 ether) in `scripts/devnet/DeployProofSystem.s.sol`.
const WORLD_PROPOSER_BOND_WEI: u128 = 1_000_000_000_000_000_000;
/// Delay between World Chain proof-system proposal attempts.
const WORLD_PROPOSER_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Bond, in wei, sent with every `WorldChainProofSystemGame.challenge`.
/// Matches `CHALLENGER_BOND` (0.1 ether) in `scripts/devnet/DeployProofSystem.s.sol`.
const WORLD_CHALLENGER_BOND_WEI: u128 = 100_000_000_000_000_000;
/// Delay between World Chain proof-system challenger scans.
const WORLD_CHALLENGER_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Delay between World Chain proof-system defender scans.
const WORLD_DEFENDER_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Balance, in wei, allocated to the World Chain challenger account in the L1
/// genesis so it can cover the challenger bond plus gas. (100 ether)
const WORLD_CHALLENGER_GENESIS_BALANCE_WEI: &str = "0x56bc75e2d63100000";
/// Balance, in wei, allocated to the World Chain defender account in the L1
/// genesis so it can submit proof-lane transactions. (100 ether)
const WORLD_DEFENDER_GENESIS_BALANCE_WEI: &str = "0x56bc75e2d63100000";

const DEVNET_PRIVATE_KEY: &str =
    "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d";
const UNSAFE_BLOCK_SIGNER_PRIVATE_KEY: &str =
    "0x92db14e403b83dfe3df233f83dfa3a0d7096f21ca9b0d6d6b8d88b2b4ec1564e";
const BATCHER_PRIVATE_KEY: &str =
    "0x4bbbf85ce3377467afe5d46f804f221813b2bb87f24d81f60f1fcdbf7cbf4356";
const PROPOSER_PRIVATE_KEY: &str =
    "0xdbda1821b80551c9d65939329250298aa3472ba22feea921c0cf5d620ea67b97";
const CHALLENGER_PRIVATE_KEY: &str =
    "0x2a871d0798f97d79848a013d4936a73bf4cc922c825d33c1cf7073dff6d409c6";
/// Signing key for the in-process World Chain proof-system challenger.
///
/// Dedicated key (address `0x743dAA55063C608894C125Cf8eC82Afe83B2d5c5`), distinct
/// from the proposer (Anvil account #1) and the op-challenger (Anvil account #9),
/// so the in-process challenger never races them on L1 nonces. The matching
/// address is funded via the L1 genesis and staked in the `MockStakingRegistry`.
const WORLD_CHALLENGER_PRIVATE_KEY: &str =
    "0x7c0c9c6f3f4d8a2b1e5d9a8c7b6e5f4a3c2d1e0f9a8b7c6d5e4f3a2b1c0d9e8f";
/// Signing key for the in-process World Chain proof-system defender.
///
/// Dedicated dev key (distinct from the proposer, challenger, and op accounts) so the
/// defender never races them on L1 nonces. The matching address is funded via the L1
/// genesis; the defender only pays gas for `submitProofLane` (no bond, no staking).
const WORLD_DEFENDER_PRIVATE_KEY: &str =
    "0x6b1f3d2c5a4e9876b0c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f60718293a4b5c6d";
/// Signing key for an e2e "griefer" that challenges a valid game to exercise the defender.
///
/// Dedicated dev key, funded via the L1 genesis. The honest challenger only challenges
/// invalid roots, so a test needs its own account to challenge a valid game and trigger a
/// defense. Exposed via [`FullStackWorldDevnet::e2e_griefer_key`] for tests only.
pub const WORLD_E2E_GRIEFER_PRIVATE_KEY: &str =
    "0x5d3a1b9e7c4f206d8a1c3e5b7d9f0a2c4e6b8d0f1a3c5e7b9d1f3a5c7e9b0d2f";
/// Balance, in wei, allocated to the e2e griefer account in the L1 genesis. (100 ether)
const WORLD_E2E_GRIEFER_GENESIS_BALANCE_WEI: &str = "0x56bc75e2d63100000";

/// Balance, in wei, prefunded to each standard devnet EOA (deployer, batcher, proposer,
/// op-challenger) in the L1 genesis `alloc`. (1,000,000 ether)
///
/// On the anvil L1 these accounts were auto-funded; the reth L1 only credits what the
/// genesis `alloc` declares, so they must be funded explicitly or the proof-system deploy
/// and op-batcher/proposer L1 transactions fail with `insufficient funds`.
const DEVNET_DEV_ACCOUNT_GENESIS_BALANCE_WEI: &str = "0xd3c21bcecceda1000000";

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
    _proof_system: Option<WorldProofSystemDeployment>,
    _world_proposer: Option<ProposerTask>,
    _world_challenger: Option<ChallengerTask>,
    _world_defender: Option<DefenderTask>,
    _prover_service: Option<ProverServiceTask>,
    _sp1_worker: Option<Sp1WorkerTask>,
    prover_service_url: Option<String>,
    _conductors: Vec<ConductorService>,
    _op_nodes: Vec<OpNodeService>,
    sequencers: Vec<SequencerService>,
    observability: Option<ObservabilityStack>,
    l1: L1Stack,
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
    p2p_host_port: u16,
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
}

#[derive(Clone, Debug)]
struct OpNodePlan {
    rpc_host_port: u16,
    metrics_host_port: u16,
    p2p_host_port: u16,
    bootnode: String,
    private_key_path: String,
}

#[derive(Debug)]
struct OpNodeService {
    id: String,
    rpc_url: String,
    p2p_host_port: u16,
    bootnodes: Vec<String>,
    metrics_target: MetricsTarget,
    image: ContainerImage,
    _container: ContainerAsync<GenericImage>,
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

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorldProofSystemDeployment {
    anchor_state_registry: String,
    validity_proof_verifier: String,
    tee_verifier: String,
    security_council: String,
    staking_registry: String,
    proof_system_factory: String,
    rollup_config_hash: String,
    l2_chain_id: u64,
    proof_system_version: u64,
    block_interval: u64,
    intermediate_block_interval: u64,
}

/// Public view of the deployed proof-system contract addresses, for tests/tools.
#[derive(Clone, Copy, Debug)]
pub struct ProofSystemInfo {
    pub factory: Address,
    pub staking_registry: Address,
    pub anchor_state_registry: Address,
    pub validity_proof_verifier: Address,
    pub block_interval: u64,
}

/// In-process World Chain proof-system proposer task. Aborted on devnet drop.
#[derive(Debug)]
struct ProposerTask {
    handle: JoinHandle<()>,
}

impl Drop for ProposerTask {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// In-process World Chain proof-system challenger task. Aborted on devnet drop.
#[derive(Debug)]
struct ChallengerTask {
    handle: JoinHandle<()>,
}

impl Drop for ChallengerTask {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// In-process World Chain proof-system defender task. Aborted on devnet drop.
#[derive(Debug)]
struct DefenderTask {
    handle: JoinHandle<()>,
}

impl Drop for DefenderTask {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// In-process defender prover-service RPC server. Stopped on devnet drop.
#[derive(Debug)]
struct ProverServiceTask {
    handle: ServerHandle,
}

impl Drop for ProverServiceTask {
    fn drop(&mut self) {
        let _ = self.handle.stop();
    }
}

/// In-process defender SP1 proving worker task. Aborted on devnet drop.
#[derive(Debug)]
struct Sp1WorkerTask {
    handle: JoinHandle<()>,
}

impl Drop for Sp1WorkerTask {
    fn drop(&mut self) {
        self.handle.abort();
    }
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

        // The CL genesis is generated from the EL genesis, so the beacon's eth1 genesis time must
        // equal the EL genesis `timestamp` (op-deployer startBlock, ~now). Read it back to keep
        // the EL and CL in agreement.
        let el_genesis: Value = read_json(&artifacts.l1_genesis_path)?;
        let genesis_timestamp = el_genesis
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|t| u64::from_str_radix(t.trim_start_matches("0x"), 16).ok())
            .ok_or_else(|| eyre!("L1 EL genesis missing/invalid timestamp"))?;

        let stable = port_mode == DevnetPortMode::Stable;
        let l1_config = L1StackConfig {
            reth_image: "ghcr.io/paradigmxyz/reth".to_string(),
            reth_tag: "v2.3.0".to_string(),
            lighthouse_image: "sigp/lighthouse".to_string(),
            lighthouse_tag: "v8.0.1".to_string(),
            genesis_generator_image: "ethpandaops/ethereum-genesis-generator".to_string(),
            genesis_generator_tag: "master".to_string(),
            chain_id: 31_337,
            slot_duration_secs: block_time.as_secs().max(1),
            genesis_timestamp,
            el_genesis_path: artifacts.l1_genesis_path.clone(),
            jwt_path: workdir_path.join("jwt.hex"),
            workdir: workdir_path.clone(),
            stable_rpc_port: stable.then_some(ANVIL_RPC_PORT),
            stable_beacon_port: stable.then_some(5052),
        };

        let l1 = L1Stack::start(l1_config)
            .await
            .wrap_err("failed to start OP-contract-backed L1 (reth + Lighthouse)")?;
        let l1_public_rpc = l1.rpc_url().to_string();
        let l1_internal_rpc = host_internal_url(l1.rpc_url())?;
        let l1_beacon_internal = host_internal_url(l1.beacon_url())?;
        // In-process services (SP1 worker) reach the L1 beacon via the host-mapped URL.
        let l1_public_beacon = l1.beacon_url().to_string();
        let actual_l1_hash = block_hash(&l1_public_rpc, 0)
            .await
            .wrap_err("failed to read Anvil L1 genesis hash")?;
        patch_rollup_l1_hash(&artifacts.rollup_path, &actual_l1_hash)?;

        info!(
            l1_rpc_url = %l1_public_rpc,
            l1_genesis_hash = %actual_l1_hash,
            "L1 dev chain has OP contracts loaded"
        );

        // Enabling the SP1 worker (via `DEVNET_SP1_WORKER_PROVER`) also switches the proof
        // system to the real SP1 Groth16 validity verifier with a single-lane threshold, so a
        // defended game finalizes on one SP1 proof. The proof-system deploy itself happens
        // *after* the L2 sequencers and conductor are healthy (see below): with the real SP1
        // path it computes vkeys and runs two forge invocations, which is slow enough that
        // doing it before L2 boot lets the L1 race ahead and the op-conductor health check
        // times out.
        let sp1_prover_kind = sp1_worker_prover_kind();

        let sequencer_count = config.sequencer_count.max(1) as usize;
        let sequencer_plans = (0..sequencer_count)
            .map(plan_sequencer)
            .collect::<Result<Vec<_>>>()
            .wrap_err("failed to plan world-chain EL peer mesh")?;
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
            start_world_chain_el(
                index,
                &workdir_path,
                &sequencer_plans[index],
                trusted_peers,
                access_list,
            )
        }))
        .await?;
        connect_execution_peers(&sequencers).await?;

        let mut conductor_plans = Vec::with_capacity(sequencer_count);
        for index in 0..sequencer_count {
            conductor_plans.push(plan_conductor(index, port_mode)?);
        }

        let op_node_plans = plan_op_nodes(sequencer_count, &workdir_path)
            .wrap_err("failed to plan op-node bootnode peer mesh")?;
        let op_node_bootnodes: Vec<_> = (0..sequencer_count)
            .map(|index| op_node_bootnodes(&op_node_plans, index))
            .collect();
        let op_nodes = try_join_all((0..sequencer_count).map(|index| {
            start_op_node(
                index,
                &workdir_path,
                &config.images.op_node,
                &op_node_plans[index],
                &op_node_bootnodes[index],
                &l1_internal_rpc,
                &l1_beacon_internal,
                &sequencers[index],
                &conductor_plans[index].rpc_url,
            )
        }))
        .await?;
        wait_for_op_node_peer_mesh(&op_nodes).await?;

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
        start_bootstrap_sequencer(&op_nodes[0]).await?;
        wait_for_l2_blocks_with_logs(
            &sequencers[0].rpc_url,
            1,
            Duration::from_secs(120),
            &op_nodes,
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
            &op_nodes,
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
                    &l1_beacon_internal,
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

        // Deploy the proof system now that the L2 sequencers and conductor are healthy. With
        // the real SP1 path this computes vkeys and runs two forge invocations (slow), so it
        // must not block L2 boot above. The proposer/challenger/defender below consume it.
        let proof_system = if config.world_contracts.proof_system {
            let sp1_vkeys = if sp1_prover_kind.is_some() {
                Some(compute_sp1_vkeys().await?)
            } else {
                None
            };
            Some(
                deploy_world_proof_system(
                    &l1_public_rpc,
                    &artifacts.rollup_path,
                    &workdir_path,
                    sp1_vkeys,
                )
                .await?,
            )
        } else {
            None
        };

        let world_proposer = if let Some(deployment) = proof_system.as_ref() {
            let output_root_rpc = op_nodes
                .first()
                .map(|node| node.rpc_url.clone())
                .ok_or_else(|| {
                    eyre!("full-stack devnet has no op-node for the World Chain proposer")
                })?;
            Some(start_world_chain_proposer(&l1_public_rpc, &output_root_rpc, deployment).await?)
        } else {
            None
        };

        let world_challenger = if let Some(deployment) = proof_system.as_ref() {
            let output_root_rpc = op_nodes
                .first()
                .map(|node| node.rpc_url.clone())
                .ok_or_else(|| {
                    eyre!("full-stack devnet has no op-node for the World Chain challenger")
                })?;
            Some(start_world_chain_challenger(&l1_public_rpc, &output_root_rpc, deployment).await?)
        } else {
            None
        };

        // Defender proving loop: an in-process defender, prover-service, and SP1 worker,
        // enabled by the `DEVNET_SP1_WORKER_PROVER` env var. The defender enqueues proof
        // requests for challenged valid games; the worker leases SP1 jobs, builds witnesses
        // from the devnet L1/L2 RPCs, and proves them with the selected backend.
        let (prover_service, sp1_worker, prover_service_url) =
            match (proof_system.as_ref(), sp1_prover_kind) {
                (Some(deployment), Some(kind)) => {
                    let l2_rpc = sequencers
                        .first()
                        .map(|sequencer| sequencer.rpc_url.clone())
                        .ok_or_else(|| {
                            eyre!("full-stack devnet has no sequencer for the SP1 worker")
                        })?;
                    let (service, url) = start_prover_service().await?;
                    let worker = start_sp1_worker(
                        &l1_public_rpc,
                        &l1_public_beacon,
                        &l2_rpc,
                        &url,
                        &artifacts.rollup_path,
                        deployment,
                        kind,
                    )
                    .await?;
                    (Some(service), Some(worker), Some(url))
                }
                _ => (None, None, None),
            };

        // Defender: requests SP1 proofs from the prover-service for challenged-but-valid games
        // and submits them on-chain. Only meaningful when the proving loop above is running.
        let world_defender = match (proof_system.as_ref(), prover_service_url.as_ref()) {
            (Some(deployment), Some(prover_service_url)) => {
                let output_root_rpc = op_nodes
                    .first()
                    .map(|node| node.rpc_url.clone())
                    .ok_or_else(|| {
                        eyre!("full-stack devnet has no op-node for the World Chain defender")
                    })?;
                Some(
                    start_world_chain_defender(
                        &l1_public_rpc,
                        &output_root_rpc,
                        prover_service_url,
                        deployment,
                    )
                    .await?,
                )
            }
            _ => None,
        };

        let mut metrics_targets = Vec::new();
        metrics_targets.extend(
            sequencers
                .iter()
                .map(|service| service.metrics_target.clone()),
        );
        metrics_targets.extend(
            op_nodes
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
            &op_nodes,
            &conductors,
            batcher.as_ref(),
            proposer.as_ref(),
            challenger.as_ref(),
            proof_system.as_ref(),
            observability.as_ref(),
            &game_factory,
            prover_service_url.as_deref(),
        );

        Ok(Self {
            _batcher: batcher,
            _proposer: proposer,
            _challenger: challenger,
            _proof_system: proof_system,
            _world_proposer: world_proposer,
            _world_challenger: world_challenger,
            _world_defender: world_defender,
            _prover_service: prover_service,
            _sp1_worker: sp1_worker,
            prover_service_url,
            _conductors: conductors,
            _op_nodes: op_nodes,
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

    /// JSON-RPC URL of the in-process defender prover-service, when enabled.
    #[allow(dead_code)]
    pub fn prover_service_url(&self) -> Option<&str> {
        self.prover_service_url.as_deref()
    }

    /// Deployed proof-system contract addresses, when the proof system is enabled.
    pub fn proof_system(&self) -> Option<ProofSystemInfo> {
        self._proof_system.as_ref().and_then(|d| {
            Some(ProofSystemInfo {
                factory: d.proof_system_factory.parse().ok()?,
                staking_registry: d.staking_registry.parse().ok()?,
                anchor_state_registry: d.anchor_state_registry.parse().ok()?,
                validity_proof_verifier: d.validity_proof_verifier.parse().ok()?,
                block_interval: d.block_interval,
            })
        })
    }

    /// Private key of a funded+unused account a test can stake and use to challenge a valid
    /// game, triggering the defender. Test-only.
    pub fn e2e_griefer_key(&self) -> &'static str {
        WORLD_E2E_GRIEFER_PRIVATE_KEY
    }

    pub fn sequencer_rpc_url(&self) -> &str {
        &self.sequencers[0].rpc_url
    }

    pub fn flashblocks_url(&self) -> &str {
        &self.sequencers[0].flashblocks_url
    }

    pub async fn safe_block_number(&self) -> Result<u64> {
        let op_node = self
            ._op_nodes
            .first()
            .ok_or_else(|| eyre!("full-stack devnet has no op-node"))?;
        let sync_status = json_rpc(&op_node.rpc_url, "optimism_syncStatus", json!([])).await?;
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

/// SP1 verification keys wired into the validity-lane verifier at deploy time.
#[derive(Clone, Copy, Debug)]
struct Sp1Vkeys {
    aggregation_vkey: alloy_primitives::B256,
    range_vkey_commitment: alloy_primitives::B256,
}

/// Deploys the standalone Succinct SP1 Groth16 verifier via `forge create`.
///
/// That contract pins solc 0.8.20 (pre-cancun), so it is compiled in isolation with explicit
/// `--use 0.8.20 --evm-version shanghai` and never enters the 0.8.28 project build. Returns
/// the deployed address, which the proof-system deploy receives via `SP1_VERIFIER_ADDRESS`.
async fn forge_create_sp1_verifier(l1_rpc_url: &str, contracts_dir: &Path) -> Result<Address> {
    let output = Command::new("forge")
        .current_dir(contracts_dir)
        .arg("create")
        .arg("lib/sp1-contracts/contracts/src/v6.1.0/SP1VerifierGroth16.sol:SP1Verifier")
        .arg("--rpc-url")
        .arg(l1_rpc_url)
        .arg("--private-key")
        .arg(DEVNET_PRIVATE_KEY)
        .arg("--use")
        .arg("0.8.20")
        .arg("--evm-version")
        .arg("shanghai")
        .arg("--broadcast")
        .arg("--json")
        .output()
        .await
        .wrap_err("failed to spawn forge create for the SP1 verifier")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        bail!(
            "forge create SP1 verifier failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            stdout,
            stderr
        );
    }

    // `forge create --json` prints a JSON object containing `deployedTo` (pretty-printed across
    // multiple lines), so extract the whole object from the first `{` to the last `}`.
    let start = stdout.find('{').ok_or_else(|| {
        eyre!("forge create produced no JSON output:\nstdout:\n{stdout}\nstderr:\n{stderr}")
    })?;
    let end = stdout
        .rfind('}')
        .ok_or_else(|| eyre!("forge create JSON was not terminated:\n{stdout}"))?;
    let parsed: Value = serde_json::from_str(&stdout[start..=end]).wrap_err_with(|| {
        format!(
            "failed to parse forge create JSON:\n{}",
            &stdout[start..=end]
        )
    })?;
    let address: Address = parsed
        .get("deployedTo")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("forge create JSON missing deployedTo: {parsed}"))?
        .parse()
        .wrap_err("invalid SP1 verifier address from forge create")?;

    info!(sp1_verifier = %address, "deployed standalone SP1 Groth16 verifier");
    Ok(address)
}

async fn deploy_world_proof_system(
    l1_rpc_url: &str,
    rollup_path: &Path,
    workdir: &Path,
    sp1_vkeys: Option<Sp1Vkeys>,
) -> Result<WorldProofSystemDeployment> {
    let rollup_config = fs::read(rollup_path)
        .wrap_err_with(|| format!("failed to read rollup config {}", rollup_path.display()))?;
    let rollup_config_hash = keccak256(&rollup_config);
    let rollup_config_hash_hex = format!("0x{}", hex::encode(rollup_config_hash.as_slice()));
    let contracts_dir = repo_root()?.join("pkg/contracts");
    let deployment_name = workdir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("world-devnet");
    let deployment_rel_path = PathBuf::from("out")
        .join("devnet")
        .join(format!("{deployment_name}-proof-system-deployment.json"));
    let deployment_path = contracts_dir.join(&deployment_rel_path);
    if let Some(parent) = deployment_path.parent() {
        fs::create_dir_all(parent).wrap_err_with(|| {
            format!(
                "failed to create proof-system deployment output directory {}",
                parent.display()
            )
        })?;
    }

    let mut command = Command::new("forge");
    command
        .current_dir(&contracts_dir)
        .arg("script")
        .arg("scripts/devnet/DeployProofSystem.s.sol:DeployProofSystem")
        .arg("--broadcast")
        .arg("--rpc-url")
        .arg(l1_rpc_url)
        .arg("--private-key")
        .arg(DEVNET_PRIVATE_KEY)
        .arg("--slow")
        .arg("--evm-version")
        .arg("cancun")
        .env("PRIVATE_KEY", DEVNET_PRIVATE_KEY)
        .env(
            "WORLD_CHALLENGER_ADDRESS",
            world_challenger_address()?.to_string(),
        )
        .env("WORLD_CHAIN_L2_CHAIN_ID", DEV_CHAIN_ID.to_string())
        .env("ROLLUP_CONFIG_HASH", &rollup_config_hash_hex)
        .env(
            "PROOF_SYSTEM_BLOCK_INTERVAL",
            PROOF_SYSTEM_BLOCK_INTERVAL.to_string(),
        )
        .env(
            "PROOF_SYSTEM_INTERMEDIATE_BLOCK_INTERVAL",
            PROOF_SYSTEM_INTERMEDIATE_BLOCK_INTERVAL.to_string(),
        )
        .env("PROOF_SYSTEM_DEPLOYMENT_OUT", &deployment_rel_path);

    // With SP1 proving enabled, deploy the real Groth16 validity verifier (keyed to the
    // committed ELFs) and finalize a defended game on a single proven lane (threshold = 1).
    // The Succinct Groth16 verifier pins solc 0.8.20, so it is deployed standalone first and
    // passed in by address; this 0.8.28 script never imports it.
    if let Some(vkeys) = sp1_vkeys {
        let sp1_verifier = forge_create_sp1_verifier(l1_rpc_url, &contracts_dir).await?;
        command
            .env("PROOF_SYSTEM_REAL_SP1", "1")
            .env("PROOF_SYSTEM_THRESHOLD", "1")
            .env("SP1_VERIFIER_ADDRESS", sp1_verifier.to_string())
            .env("AGGREGATION_VKEY", vkeys.aggregation_vkey.to_string())
            .env(
                "RANGE_VKEY_COMMITMENT",
                vkeys.range_vkey_commitment.to_string(),
            );
    }

    info!(
        l1_rpc_url,
        rollup_config_hash = %rollup_config_hash_hex,
        real_sp1 = sp1_vkeys.is_some(),
        output = %deployment_path.display(),
        "deploying World Chain proof-system contracts"
    );

    let output = command
        .output()
        .await
        .wrap_err("failed to spawn forge proof-system deployment")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.trim().is_empty() {
        emit_command_logs(
            "world-proof-system deploy",
            ProcessLogTarget::OpDeployer,
            &stdout,
        );
    }
    if !stderr.trim().is_empty() {
        emit_command_logs(
            "world-proof-system deploy",
            ProcessLogTarget::OpDeployer,
            &stderr,
        );
    }
    if !output.status.success() {
        bail!(
            "forge proof-system deployment failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            stdout,
            stderr
        );
    }

    let deployment: WorldProofSystemDeployment =
        serde_json::from_value(read_json(&deployment_path)?).wrap_err_with(|| {
            format!(
                "invalid World Chain proof-system deployment JSON at {}",
                deployment_path.display()
            )
        })?;

    info!(
        factory = %deployment.proof_system_factory,
        anchor = %deployment.anchor_state_registry,
        validity = %deployment.validity_proof_verifier,
        tee = %deployment.tee_verifier,
        council = %deployment.security_council,
        "World Chain proof-system contracts deployed"
    );

    Ok(deployment)
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
    let mut alloc: Value =
        serde_json::from_str(&alloc_json).wrap_err("invalid l1StateDump JSON")?;
    fund_world_challenger(&mut alloc)?;
    fund_world_defender(&mut alloc)?;
    fund_world_e2e_griefer(&mut alloc)?;
    fund_devnet_dev_accounts(&mut alloc)?;
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

/// Returns the address used by the in-process World Chain proof-system challenger.
fn world_challenger_address() -> Result<Address> {
    let signer: PrivateKeySigner = WORLD_CHALLENGER_PRIVATE_KEY
        .parse()
        .wrap_err("invalid World Chain challenger signing key")?;
    Ok(signer.address())
}

fn world_defender_address() -> Result<Address> {
    let signer: PrivateKeySigner = WORLD_DEFENDER_PRIVATE_KEY
        .parse()
        .wrap_err("invalid World Chain defender signing key")?;
    Ok(signer.address())
}

/// Funds the World Chain defender account in the L1 genesis `alloc` so it can pay gas
/// for `submitProofLane`. Dedicated account, never present in the op-deployer dump.
fn fund_world_defender(alloc: &mut Value) -> Result<()> {
    let address = world_defender_address()?;
    let entry = alloc
        .as_object_mut()
        .ok_or_else(|| eyre!("l1 genesis alloc is not a JSON object"))?;
    entry.insert(
        address.to_string(),
        json!({ "balance": WORLD_DEFENDER_GENESIS_BALANCE_WEI }),
    );
    Ok(())
}

fn world_e2e_griefer_address() -> Result<Address> {
    let signer: PrivateKeySigner = WORLD_E2E_GRIEFER_PRIVATE_KEY
        .parse()
        .wrap_err("invalid World Chain e2e griefer signing key")?;
    Ok(signer.address())
}

/// Funds the e2e griefer account in the L1 genesis so a test can stake it and challenge a
/// valid game (covering the challenger bond plus gas).
fn fund_world_e2e_griefer(alloc: &mut Value) -> Result<()> {
    let address = world_e2e_griefer_address()?;
    let entry = alloc
        .as_object_mut()
        .ok_or_else(|| eyre!("l1 genesis alloc is not a JSON object"))?;
    entry.insert(
        address.to_string(),
        json!({ "balance": WORLD_E2E_GRIEFER_GENESIS_BALANCE_WEI }),
    );
    Ok(())
}

/// Funds the World Chain challenger account in the L1 genesis `alloc` so it can
/// pay the challenger bond (plus gas) when submitting challenges. The account is
/// dedicated to the challenger, so it is never present in the op-deployer dump.
fn fund_world_challenger(alloc: &mut Value) -> Result<()> {
    let address = world_challenger_address()?;
    let entry = alloc
        .as_object_mut()
        .ok_or_else(|| eyre!("l1 genesis alloc is not a JSON object"))?;
    entry.insert(
        address.to_string(),
        json!({ "balance": WORLD_CHALLENGER_GENESIS_BALANCE_WEI }),
    );
    Ok(())
}

/// Prefunds the standard devnet EOAs (deployer, batcher, proposer, op-challenger) in the L1
/// genesis `alloc`. The reth L1 only credits accounts declared in genesis, so without this the
/// `forge create`/proof-system deploys (DEVNET key) and the op-batcher/op-proposer L1
/// transactions fail with `insufficient funds`. Existing dump entries (e.g. contract accounts)
/// are left untouched; only the balance of these EOAs is set.
fn fund_devnet_dev_accounts(alloc: &mut Value) -> Result<()> {
    let keys = [
        DEVNET_PRIVATE_KEY,
        BATCHER_PRIVATE_KEY,
        PROPOSER_PRIVATE_KEY,
        CHALLENGER_PRIVATE_KEY,
    ];
    let entry = alloc
        .as_object_mut()
        .ok_or_else(|| eyre!("l1 genesis alloc is not a JSON object"))?;
    for key in keys {
        let signer: PrivateKeySigner = key
            .parse()
            .wrap_err("invalid devnet dev-account signing key")?;
        // Merge the balance into any existing dump entry (e.g. the deployer's nonce/code)
        // rather than replacing it, so we don't drop state op-deployer baked in.
        match entry.entry(signer.address().to_string()) {
            serde_json::map::Entry::Occupied(mut existing) => {
                if let Some(obj) = existing.get_mut().as_object_mut() {
                    obj.insert(
                        "balance".to_string(),
                        json!(DEVNET_DEV_ACCOUNT_GENESIS_BALANCE_WEI),
                    );
                }
            }
            serde_json::map::Entry::Vacant(slot) => {
                slot.insert(json!({ "balance": DEVNET_DEV_ACCOUNT_GENESIS_BALANCE_WEI }));
            }
        }
    }
    Ok(())
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
    Ok(SequencerPlan {
        rpc_host_port: reserve_host_port()?,
        ws_host_port: reserve_host_port()?,
        auth_host_port: reserve_host_port()?,
        metrics_host_port: reserve_host_port()?,
        p2p_host_port,
        p2p_secret_key,
        trusted_peer,
    })
}

async fn start_world_chain_el(
    index: usize,
    workdir: &Path,
    plan: &SequencerPlan,
    trusted_peers: Vec<String>,
    access_list: bool,
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
        "--ipcdisable".to_string(),
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
        // Serve historical `eth_getProof` so the SP1 worker can build witnesses for past
        // blocks (defaults to 0 = tip only, which fails witness generation). Use reth's max
        // (`MAX_ETH_PROOF_WINDOW` = 1_209_600) so deep ranges stay provable.
        "--rpc.eth-proof-window".to_string(),
        "1209600".to_string(),
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

    info!(
        index,
        rpc_url = %rpc_url,
        auth_url = %auth_url,
        p2p = %format!("127.0.0.1:{p2p_port}"),
        metrics = %format!("127.0.0.1:{metrics_port}"),
        binary = %binary.display(),
        "native world-chain EL started"
    );

    Ok(SequencerService {
        id: format!("world-chain-el-{index}"),
        rpc_url,
        ws_url,
        auth_url,
        flashblocks_url: format!("ws://127.0.0.1:{ws_port}"),
        p2p_host_port: p2p_port,
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

fn plan_op_nodes(count: usize, workdir: &Path) -> Result<Vec<OpNodePlan>> {
    let mut plans = Vec::with_capacity(count);
    for index in 0..count {
        let private_key = random_p2p_secret_key();
        let p2p_host_port = reserve_host_port()?;
        let filename = format!("op-node-{index}-p2p-priv.txt");
        fs::write(workdir.join(&filename), &private_key)
            .wrap_err_with(|| format!("failed to write op-node P2P key {filename}"))?;
        plans.push(OpNodePlan {
            rpc_host_port: 19_545 + index as u16,
            metrics_host_port: reserve_host_port()?,
            p2p_host_port,
            bootnode: devnet_trusted_peer(&private_key, "host.docker.internal", p2p_host_port)?,
            private_key_path: format!("/work/{filename}"),
        });
    }
    Ok(plans)
}

fn op_node_bootnodes(plans: &[OpNodePlan], source_index: usize) -> Vec<String> {
    plans
        .iter()
        .enumerate()
        .filter(|&(target_index, _target)| target_index != source_index)
        .map(|(_target_index, target)| target.bootnode.clone())
        .collect()
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
    let node_rpc = format!("http://host.docker.internal:{}", 19_545 + index as u16);
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

async fn start_op_node(
    index: usize,
    workdir: &Path,
    image: &ContainerImage,
    plan: &OpNodePlan,
    bootnodes: &[String],
    l1_rpc: &str,
    l1_beacon: &str,
    sequencer: &SequencerService,
    conductor_rpc_url: &str,
) -> Result<OpNodeService> {
    let conductor_rpc = host_internal_url(conductor_rpc_url)?;
    let l2_engine_rpc = host_internal_url(&sequencer.auth_url)?;
    let p2p_host_port = plan.p2p_host_port.to_string();
    let mut cmd = vec![
        "-vvv".to_string(),
        "--logs.stdout.format=logfmt".to_string(),
        "node".to_string(),
        "--chain".to_string(),
        DEV_CHAIN_ID.to_string(),
        "--metrics.enabled".to_string(),
        "--metrics.addr".to_string(),
        "0.0.0.0".to_string(),
        "--metrics.port".to_string(),
        OP_NODE_METRICS_PORT.to_string(),
        "--mode".to_string(),
        "Sequencer".to_string(),
        "--sequencer.stopped".to_string(),
        "--sequencer.max-safe-lag".to_string(),
        "0".to_string(),
        "--sequencer.l1-confs".to_string(),
        "0".to_string(),
        "--conductor.rpc".to_string(),
        conductor_rpc,
        "--conductor.rpc.timeout".to_string(),
        "5".to_string(),
        "--l2-config-file".to_string(),
        "/work/rollup.json".to_string(),
        "--l1-config-file".to_string(),
        "/work/l1-genesis.json".to_string(),
        "--l1-eth-rpc".to_string(),
        l1_rpc.to_string(),
        "--l1-beacon".to_string(),
        l1_beacon.to_string(),
        "--l1-trust-rpc".to_string(),
        "--l2-engine-rpc".to_string(),
        l2_engine_rpc,
        "--l2-engine-jwt-secret".to_string(),
        "/work/jwt.hex".to_string(),
        "--l2-trust-rpc".to_string(),
        "--p2p.sequencer.key".to_string(),
        UNSAFE_BLOCK_SIGNER_PRIVATE_KEY.to_string(),
        "--p2p.listen.ip".to_string(),
        "0.0.0.0".to_string(),
        "--p2p.listen.tcp".to_string(),
        OP_NODE_P2P_PORT.to_string(),
        "--p2p.listen.udp".to_string(),
        OP_NODE_P2P_PORT.to_string(),
        "--p2p.advertise.ip".to_string(),
        "host.docker.internal".to_string(),
        "--p2p.advertise.tcp".to_string(),
        p2p_host_port.clone(),
        "--p2p.advertise.udp".to_string(),
        p2p_host_port,
        "--p2p.priv.path".to_string(),
        plan.private_key_path.clone(),
        "--p2p.bootstore".to_string(),
        format!("/work/kona-bootstore-{index}"),
        "--p2p.no-discovery".to_string(),
        "--p2p.redial".to_string(),
        "0".to_string(),
        "--rpc.addr".to_string(),
        "0.0.0.0".to_string(),
        "--rpc.enable-admin".to_string(),
        "--port".to_string(),
        OP_NODE_RPC_PORT.to_string(),
    ];
    if !bootnodes.is_empty() {
        cmd.push("--p2p.bootnodes".to_string());
        cmd.push(bootnodes.join(","));
    }

    let mut request = GenericImage::new(image.repository.clone(), image.tag.clone())
        .with_entrypoint("kona-node")
        .with_wait_for(WaitFor::seconds(5))
        .with_exposed_port(OP_NODE_RPC_PORT.tcp())
        .with_exposed_port(OP_NODE_METRICS_PORT.tcp())
        .with_exposed_port(OP_NODE_P2P_PORT.tcp())
        .with_exposed_port(OP_NODE_P2P_PORT.udp())
        .with_log_consumer(container_log_consumer(
            format!("op-node-{index}"),
            ProcessLogTarget::OpNode,
        ))
        .with_cmd(cmd)
        .with_startup_timeout(Duration::from_secs(120))
        .with_mount(Mount::bind_mount(
            workdir.to_string_lossy().to_string(),
            "/work",
        ));
    request = request.with_mapped_port(plan.rpc_host_port, OP_NODE_RPC_PORT.tcp());
    request = request.with_mapped_port(plan.metrics_host_port, OP_NODE_METRICS_PORT.tcp());
    request = request.with_mapped_port(plan.p2p_host_port, OP_NODE_P2P_PORT.tcp());
    request = request.with_mapped_port(plan.p2p_host_port, OP_NODE_P2P_PORT.udp());

    let container = request
        .start()
        .await
        .wrap_err_with(|| format!("failed to start op-node {index}"))?;

    let host = container.get_host().await?;
    let rpc_url = format!("http://{host}:{}", plan.rpc_host_port);
    if let Err(err) = wait_for_json_rpc(
        &rpc_url,
        "optimism_rollupConfig",
        json!([]),
        Duration::from_secs(60),
    )
    .await
    {
        let logs = container_logs(&container).await;
        return Err(err).wrap_err_with(|| {
            format!("op-node {index} RPC did not become ready; container logs:\n{logs}")
        });
    }

    Ok(OpNodeService {
        id: format!("op-node-{index}"),
        rpc_url,
        p2p_host_port: plan.p2p_host_port,
        bootnodes: bootnodes.to_vec(),
        metrics_target: MetricsTarget::new(
            format!("op-node-{index}"),
            format!("host.docker.internal:{}", plan.metrics_host_port),
        ),
        image: image.clone(),
        _container: container,
    })
}

async fn wait_for_op_node_peer_mesh(op_nodes: &[OpNodeService]) -> Result<()> {
    if op_nodes.len() <= 1 {
        return Ok(());
    }

    let expected = op_nodes.len().saturating_sub(1) as u64;
    info!(
        nodes = op_nodes.len(),
        expected_peers_per_node = expected,
        "waiting for op-node bootnode peer mesh"
    );
    for node in op_nodes {
        retry_until(
            Duration::from_secs(30),
            Duration::from_millis(500),
            || async {
                let peers = json_rpc(&node.rpc_url, "opp2p_peers", json!([true])).await?;
                let connected = peers
                    .get("totalConnected")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        eyre!(
                            "opp2p_peers for {} missing totalConnected: {peers}",
                            node.id
                        )
                    })?;
                if connected == expected {
                    Ok(())
                } else {
                    bail!(
                        "{} has {connected} connected op-node peers, expected {expected}",
                        node.id
                    )
                }
            },
        )
        .await
        .wrap_err_with(|| format!("op-node bootnode P2P mesh did not form for {}", node.id))?;
    }

    info!(count = op_nodes.len(), "op-node P2P mesh connected");
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

async fn start_bootstrap_sequencer(op_node: &OpNodeService) -> Result<()> {
    retry_until(
        Duration::from_secs(30),
        Duration::from_millis(500),
        || async {
            let active = json_rpc(&op_node.rpc_url, "admin_sequencerActive", json!([]))
                .await?
                .as_bool()
                .ok_or_else(|| eyre!("admin_sequencerActive did not return a bool"))?;
            if active {
                return Ok(());
            }

            let sync_status = json_rpc(&op_node.rpc_url, "optimism_syncStatus", json!([])).await?;
            let unsafe_hash = sync_status
                .pointer("/unsafe_l2/hash")
                .and_then(Value::as_str)
                .ok_or_else(|| eyre!("optimism_syncStatus missing unsafe_l2.hash: {sync_status}"))?
                .to_string();
            json_rpc(
                &op_node.rpc_url,
                "admin_startSequencer",
                json!([unsafe_hash]),
            )
            .await
            .map(|_| ())
        },
    )
    .await
    .wrap_err_with(|| {
        format!(
            "failed to explicitly start bootstrap sequencer {}",
            op_node.id
        )
    })?;

    info!(
        op_node = %op_node.id,
        "bootstrap op-node sequencer started"
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
    l1_beacon: &str,
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
        l1_beacon.to_string(),
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

/// Spawns the in-process World Chain proof-system proposer.
///
/// The proposer signs with the dev proposer key (Anvil account #1), which
/// `DeployProofSystem.s.sol` stakes in the `MockStakingRegistry` and funds via
/// `fundDevAccounts`. Output roots are read from the op-node rollup RPC and
/// proposals are submitted to `WorldChainProofSystemFactory.propose` on L1.
async fn start_world_chain_proposer(
    l1_rpc_url: &str,
    output_root_rpc_url: &str,
    deployment: &WorldProofSystemDeployment,
) -> Result<ProposerTask> {
    let factory_address: Address = deployment
        .proof_system_factory
        .parse()
        .wrap_err("invalid proof-system factory address")?;
    let anchor_address: Address = deployment
        .anchor_state_registry
        .parse()
        .wrap_err("invalid anchor-state-registry address")?;

    let signer: PrivateKeySigner = DEVNET_PRIVATE_KEY
        .parse()
        .wrap_err("invalid World Chain proposer signing key")?;
    let proposer_address = signer.address();
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer))
        .connect_http(Url::parse(l1_rpc_url)?);

    let contracts = AlloyProofSystemClient::new(provider, factory_address, anchor_address);
    let output_roots = OptimismConsensusClient::new(output_root_rpc_url.to_string());
    let config = ProposerConfig {
        block_interval: deployment.block_interval,
        proposer_bond: U256::from(WORLD_PROPOSER_BOND_WEI),
        poll_interval: WORLD_PROPOSER_POLL_INTERVAL,
    };
    let proposer = WorldChainProposer::new(config, contracts, output_roots);

    info!(
        l1_rpc_url,
        output_root_rpc_url,
        factory = %deployment.proof_system_factory,
        anchor = %deployment.anchor_state_registry,
        proposer = %proposer_address,
        block_interval = deployment.block_interval,
        "starting native World Chain proof-system proposer"
    );

    let handle = tokio::spawn(
        async move {
            if let Err(error) = proposer.run_forever().await {
                warn!(%error, "World Chain proof-system proposer stopped");
            }
        }
        .instrument(info_span!(
            "world-chain-proposer",
            process = "world-chain-proposer"
        )),
    );

    Ok(ProposerTask { handle })
}

/// Spawns the in-process World Chain proof-system challenger.
///
/// The challenger signs with [`WORLD_CHALLENGER_PRIVATE_KEY`], a dedicated dev
/// account that is funded through the L1 genesis (see [`fund_world_challenger`])
/// and staked in the `MockStakingRegistry` by `DeployProofSystem.s.sol`. It scans
/// `WorldChainProofSystemFactory.GameCreated` events, recomputes the expected
/// output root from the op-node rollup RPC, and challenges any game whose
/// `rootClaim` disagrees by calling `WorldChainProofSystemGame.challenge` on L1.
async fn start_world_chain_challenger(
    l1_rpc_url: &str,
    output_root_rpc_url: &str,
    deployment: &WorldProofSystemDeployment,
) -> Result<ChallengerTask> {
    let factory_address: Address = deployment
        .proof_system_factory
        .parse()
        .wrap_err("invalid proof-system factory address")?;

    let signer: PrivateKeySigner = WORLD_CHALLENGER_PRIVATE_KEY
        .parse()
        .wrap_err("invalid World Chain challenger signing key")?;
    let challenger_address = signer.address();
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer))
        .connect_http(Url::parse(l1_rpc_url)?);

    let client = AlloyChallengerClient::new(provider, factory_address);
    let output_roots = OptimismConsensusClient::new(output_root_rpc_url.to_string());
    let config = ChallengerConfig {
        challenger_bond: U256::from(WORLD_CHALLENGER_BOND_WEI),
        poll_interval: WORLD_CHALLENGER_POLL_INTERVAL,
        ..ChallengerConfig::default()
    };
    let mut challenger = WorldChainChallenger::new(config, client, output_roots);

    info!(
        l1_rpc_url,
        output_root_rpc_url,
        factory = %deployment.proof_system_factory,
        challenger = %challenger_address,
        "starting native World Chain proof-system challenger"
    );

    let handle = tokio::spawn(
        async move {
            if let Err(error) = challenger.run_forever().await {
                warn!(%error, "World Chain proof-system challenger stopped");
            }
        }
        .instrument(info_span!(
            "world-chain-challenger",
            process = "world-chain-challenger"
        )),
    );

    Ok(ChallengerTask { handle })
}

/// Spawns the in-process World Chain proof-system defender.
///
/// The defender signs with [`WORLD_DEFENDER_PRIVATE_KEY`], a dedicated dev
/// account that is funded through the L1 genesis. It watches challenged valid
/// `WorldChainProofSystemFactory` games, requests proofs from the
/// prover-service, and submits completed proof lanes on L1.
async fn start_world_chain_defender(
    l1_rpc_url: &str,
    output_root_rpc_url: &str,
    prover_service_url: &str,
    deployment: &WorldProofSystemDeployment,
) -> Result<DefenderTask> {
    let factory_address: Address = deployment
        .proof_system_factory
        .parse()
        .wrap_err("invalid proof-system factory address")?;

    let signer: PrivateKeySigner = WORLD_DEFENDER_PRIVATE_KEY
        .parse()
        .wrap_err("invalid World Chain defender signing key")?;
    let defender_address = signer.address();
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer))
        .connect_http(Url::parse(l1_rpc_url)?);

    let client = AlloyDefenderClient::new(provider, factory_address);
    let output_roots = OptimismConsensusClient::new(output_root_rpc_url.to_string());
    let proof_requester = RpcProverServiceClient::new(prover_service_url)
        .map_err(|error| eyre!("failed to connect defender to prover-service: {error}"))?;
    let config = DefenderConfig {
        poll_interval: WORLD_DEFENDER_POLL_INTERVAL,
        ..DefenderConfig::default()
    };
    let mut defender = WorldChainDefender::new(config, client, output_roots, proof_requester);

    info!(
        l1_rpc_url,
        output_root_rpc_url,
        prover_service = %prover_service_url,
        factory = %deployment.proof_system_factory,
        defender = %defender_address,
        "starting native World Chain proof-system defender"
    );

    let handle = tokio::spawn(
        async move {
            if let Err(error) = defender.run_forever().await {
                warn!(%error, "World Chain proof-system defender stopped");
            }
        }
        .instrument(info_span!(
            "world-chain-defender",
            process = "world-chain-defender"
        )),
    );

    Ok(DefenderTask { handle })
}

/// Reads the SP1 worker prover backend from [`SP1_WORKER_PROVER_ENV`], or `None` when the
/// defender proving loop is disabled.
fn sp1_worker_prover_kind() -> Option<Sp1ProverKind> {
    std::env::var(SP1_WORKER_PROVER_ENV)
        .ok()
        .and_then(|value| value.parse().ok())
}

/// Computes the aggregation and range verification keys from the committed SP1 ELFs, for
/// wiring into the on-chain validity verifier at deploy time.
///
/// vkeys are a function of the program ELF, not the proving backend, so this uses the mock
/// prover for a fast setup — the values match what the CPU/network worker commits.
async fn compute_sp1_vkeys() -> Result<Sp1Vkeys> {
    let range_elf_path = repo_root()?.join("proofs/succinct/elf/world-chain-range-ethereum");
    let agg_elf_path = repo_root()?.join("proofs/succinct/elf/world-chain-aggregation");
    let range_elf = fs::read(&range_elf_path)
        .wrap_err_with(|| format!("failed to read range ELF {}", range_elf_path.display()))?;
    let agg_elf = fs::read(&agg_elf_path)
        .wrap_err_with(|| format!("failed to read aggregation ELF {}", agg_elf_path.display()))?;

    let vkeys = tokio::task::spawn_blocking(move || -> Result<Sp1Vkeys> {
        let prover = EnvSuccinctProver::new(
            Sp1ProverKind::Mock,
            range_elf,
            agg_elf,
            SP1ProofMode::Groth16,
        )
        .map_err(|error| eyre!("failed to set up SP1 prover for vkey computation: {error}"))?;
        Ok(Sp1Vkeys {
            aggregation_vkey: prover.aggregation_vkey(),
            range_vkey_commitment: prover.range_vkey_commitment(),
        })
    })
    .await
    .wrap_err("SP1 vkey computation task panicked")??;

    info!(
        aggregation_vkey = %vkeys.aggregation_vkey,
        range_vkey_commitment = %vkeys.range_vkey_commitment,
        "computed SP1 vkeys for proof-system deploy"
    );
    Ok(vkeys)
}

/// Starts the in-process defender prover-service and returns its task handle and JSON-RPC URL.
async fn start_prover_service() -> Result<(ProverServiceTask, String)> {
    let service = Arc::new(
        ProverService::new(ProverServiceConfig::default())
            .wrap_err("invalid prover-service config")?,
    );
    let (addr, handle) =
        start_rpc_server("127.0.0.1:0".parse().expect("valid loopback addr"), service)
            .await
            .wrap_err("failed to start prover-service RPC server")?;
    let url = format!("http://{addr}");
    info!(prover_service = %url, "started native defender prover-service");
    Ok((ProverServiceTask { handle }, url))
}

/// Spawns the in-process SP1 proving worker.
///
/// It leases SP1 jobs from the prover-service at `prover_service_url`, builds range witnesses
/// from the devnet L1/L2 RPCs (the L1 dev chain doubles as the beacon endpoint, matching the
/// op-node configuration), proves them with the selected backend, and submits the proofs back.
async fn start_sp1_worker(
    l1_rpc_url: &str,
    l1_beacon_url: &str,
    l2_rpc_url: &str,
    prover_service_url: &str,
    rollup_path: &Path,
    deployment: &WorldProofSystemDeployment,
    kind: Sp1ProverKind,
) -> Result<Sp1WorkerTask> {
    let rollup_config: Value = read_json(rollup_path)?;

    // The devnet L1 chain id (31337) is not in kona's bundled `L1_CONFIGS`, so the guest falls
    // back to requesting the L1 chain config via the local preimage key `L1_CONFIG_KEY`. The kona
    // host can only serve that from an L1 config file (`alloy_genesis::ChainConfig`, i.e. the
    // geth-genesis `config` object). Without it, witness collection busy-spins forever in
    // `get_preimage`. Extract `config` from the devnet L1 genesis and write it standalone.
    let l1_config_path = {
        let l1_genesis_path = rollup_path
            .parent()
            .map(|dir| dir.join("l1-genesis.json"))
            .ok_or_else(|| {
                eyre!(
                    "cannot derive workdir from rollup path {}",
                    rollup_path.display()
                )
            })?;
        let l1_genesis: Value = read_json(&l1_genesis_path)?;
        let l1_config = l1_genesis
            .get("config")
            .ok_or_else(|| eyre!("l1-genesis.json missing `config` object"))?;
        let out = l1_genesis_path.with_file_name("l1-chain-config.json");
        fs::write(&out, serde_json::to_vec(l1_config)?)
            .wrap_err("failed to write L1 chain config for the SP1 worker")?;
        out
    };

    // The L1 is now reth + Lighthouse, so the beacon (`l1_beacon_url`) serves
    // `/eth/v1/beacon/genesis` and `/eth/v1/config/spec` — no slot-duration override needed.
    let host = OnlineHostConfig::from_rollup_config_value(
        &rollup_config,
        l1_rpc_url.to_string(),
        l1_beacon_url.to_string(),
        l2_rpc_url.to_string(),
        Some(rollup_path.to_path_buf()),
        Duration::from_secs(900),
    )
    .map_err(|error| eyre!("failed to build SP1 worker host config: {error}"))?
    .with_l1_config_path(l1_config_path);

    let range_elf_path = repo_root()?.join("proofs/succinct/elf/world-chain-range-ethereum");
    let agg_elf_path = repo_root()?.join("proofs/succinct/elf/world-chain-aggregation");
    let range_elf = fs::read(&range_elf_path)
        .wrap_err_with(|| format!("failed to read range ELF {}", range_elf_path.display()))?;
    let agg_elf = fs::read(&agg_elf_path)
        .wrap_err_with(|| format!("failed to read aggregation ELF {}", agg_elf_path.display()))?;

    // `EnvSuccinctProver` owns its own runtime, so build it off the async runtime.
    let prover = tokio::task::spawn_blocking(move || {
        EnvSuccinctProver::new(kind, range_elf, agg_elf, SP1ProofMode::Groth16)
    })
    .await
    .wrap_err("SP1 prover setup task panicked")?
    .map_err(|error| eyre!("failed to build SP1 prover: {error}"))?;

    let backend = Sp1Backend::new(
        host,
        prover,
        Sp1BackendConfig {
            block_interval: deployment.block_interval,
            split_count: 1,
            prover_address: Address::ZERO,
            allow_unfinalized: false,
        },
    );

    let queue = RpcProverServiceClient::new(prover_service_url)
        .map_err(|error| eyre!("failed to connect SP1 worker to prover-service: {error}"))?;
    let worker = ProofWorker::new(
        queue,
        backend,
        ProofWorkerConfig {
            poll_interval: SP1_WORKER_POLL_INTERVAL,
            max_concurrent_jobs: 1,
        },
    );

    info!(
        prover_service = %prover_service_url,
        block_interval = deployment.block_interval,
        prover = ?kind,
        "starting native defender SP1 worker"
    );

    let handle = tokio::spawn(worker.instrument(info_span!("sp1-worker", process = "sp1-worker")));
    Ok(Sp1WorkerTask { handle })
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
    op_nodes: &[OpNodeService],
    conductors: &[ConductorService],
    batcher: Option<&ContainerService>,
    proposer: Option<&ContainerService>,
    challenger: Option<&ContainerService>,
    proof_system: Option<&WorldProofSystemDeployment>,
    observability: Option<&ObservabilityStack>,
    game_factory: &str,
    prover_service_url: Option<&str>,
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
            .with_endpoint("p2p", format!("127.0.0.1:{}", service.p2p_host_port))
            .with_note(
                "native direct-sequencing World Chain execution node with flashblocks enabled and trusted EL peers",
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

    for service in op_nodes {
        components.push(
            DevnetComponent::new(
                service.id.clone(),
                DevnetComponentKind::OpNode,
                DevnetComponentStatus::Running,
            )
            .with_image(service.image.clone())
            .with_endpoint("rpc", service.rpc_url.clone())
            .with_endpoint(
                "p2p",
                format!("host.docker.internal:{}", service.p2p_host_port),
            )
            .with_note(format!("bootnodes={}", service.bootnodes.join(","))),
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

    if let Some(deployment) = proof_system {
        components.push(
            DevnetComponent::new(
                "world-proof-system",
                DevnetComponentKind::WorldProofSystem,
                DevnetComponentStatus::Running,
            )
            .with_endpoint("factory", deployment.proof_system_factory.clone())
            .with_endpoint("anchor", deployment.anchor_state_registry.clone())
            .with_endpoint(
                "validity-verifier",
                deployment.validity_proof_verifier.clone(),
            )
            .with_endpoint("tee-verifier", deployment.tee_verifier.clone())
            .with_endpoint("security-council", deployment.security_council.clone())
            .with_endpoint("staking-registry", deployment.staking_registry.clone())
            .with_note(format!(
                "WIP-1006 threshold {PROOF_THRESHOLD}/3, proof_system_version={}",
                deployment.proof_system_version
            ))
            .with_note(format!(
                "l2_chain_id={}, block_interval={}, intermediate_block_interval={}",
                deployment.l2_chain_id,
                deployment.block_interval,
                deployment.intermediate_block_interval
            ))
            .with_note(format!(
                "rollup_config_hash={}",
                deployment.rollup_config_hash
            )),
        );
        components.push(
            DevnetComponent::new(
                "world-chain-proposer",
                DevnetComponentKind::WorldChainProposer,
                DevnetComponentStatus::Running,
            )
            .with_endpoint("factory", deployment.proof_system_factory.clone())
            .with_endpoint("anchor", deployment.anchor_state_registry.clone())
            .with_note(format!(
                "native in-process proposer posting OP output roots every {} L2 blocks via WorldChainProofSystemFactory.propose",
                deployment.block_interval
            ))
            .with_note("signs with the dev proposer key staked in the MockStakingRegistry"),
        );
        components.push(
            DevnetComponent::new(
                "world-chain-challenger",
                DevnetComponentKind::WorldChainChallenger,
                DevnetComponentStatus::Running,
            )
            .with_endpoint("factory", deployment.proof_system_factory.clone())
            .with_note(
                "native in-process challenger that disputes invalid WorldChainProofSystemFactory games via WorldChainProofSystemGame.challenge",
            )
            .with_note(
                "signs with a dedicated dev key funded in the L1 genesis and staked in the MockStakingRegistry",
            ),
        );
    }

    if let (Some(url), Some(deployment)) = (prover_service_url, proof_system) {
        components.push(
            DevnetComponent::new(
                "world-chain-defender",
                DevnetComponentKind::WorldChainDefender,
                DevnetComponentStatus::Running,
            )
            .with_endpoint("factory", deployment.proof_system_factory.clone())
            .with_endpoint("prover-service", url.to_string())
            .with_note(
                "native in-process defender that requests proof lanes for challenged valid WIP-1006 games",
            )
            .with_note("signs with a dedicated dev key funded in the L1 genesis"),
        );
        components.push(
            DevnetComponent::new(
                "prover-service",
                DevnetComponentKind::ProverService,
                DevnetComponentStatus::Running,
            )
            .with_endpoint("rpc", url.to_string())
            .with_note(
                "in-memory defender proof request queue; enqueue SP1/TEE jobs via the prover JSON-RPC",
            ),
        );
        components.push(
            DevnetComponent::new(
                "sp1-worker",
                DevnetComponentKind::Sp1Worker,
                DevnetComponentStatus::Running,
            )
            .with_endpoint("prover-service", url.to_string())
            .with_note(format!(
                "native in-process SP1 worker (backend from {SP1_WORKER_PROVER_ENV}); leases jobs, builds witnesses, proves, and submits back"
            )),
        );
    }

    components.push(
        DevnetComponent::new(
            "world-contracts-deployer",
            DevnetComponentKind::WorldContractsDeployer,
            if proof_system.is_some() {
                DevnetComponentStatus::Running
            } else {
                DevnetComponentStatus::Deferred
            },
        )
        .with_note(if proof_system.is_some() {
            "deployed the WIP-1006 proof-system suite to the local L1"
        } else {
            "not run by the native devnet; FeeEscrow and FeeRecipient are intentionally not deployed"
        })
        .with_note(format!(
            "proof_system_version={PROOF_SYSTEM_VERSION}, threshold={PROOF_THRESHOLD}/3"
        ))
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
    op_nodes: &[OpNodeService],
    conductors: &[ConductorService],
) -> Result<()> {
    if let Err(err) = wait_for_l2_blocks(rpc_url, min_block, timeout).await {
        let mut diagnostics = String::new();
        diagnostics.push_str("op-node status:\n");
        for node in op_nodes {
            let sync_status = json_rpc(&node.rpc_url, "optimism_syncStatus", json!([]))
                .await
                .map(|value| value.to_string())
                .unwrap_or_else(|err| format!("error: {err}"));
            let sequencer_active = json_rpc(&node.rpc_url, "admin_sequencerActive", json!([]))
                .await
                .map(|value| value.to_string())
                .unwrap_or_else(|err| format!("error: {err}"));
            let logs = tail_text(&container_logs(&node._container).await, 160);
            diagnostics.push_str(&format!(
                "\n{} rpc={} sequencer_active={} sync_status={}\nlogs:\n{}\n",
                node.id, node.rpc_url, sequencer_active, sync_status, logs
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
