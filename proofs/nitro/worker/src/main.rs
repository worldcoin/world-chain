//! `nitro-worker` binary: leases Nitro TEE proof jobs from the `prover-service`, proves
//! them inside a running Nitro Enclave, and submits the signed attestations back.
//!
//! # Architecture
//!
//! ```text
//!  ┌──────────────────────────────────────────────────────────────┐
//!  │                     nitro-worker                             │
//!  │                                                              │
//!  │  poll prover_getNextProof(Nitro)  ← generic ProofWorker     │
//!  │       │                                                      │
//!  │       ▼                                                      │
//!  │  build Kona witness over RPC (same path as bin/proof)        │
//!  │       │                                                      │
//!  │       ▼                                                      │
//!  │  NitroProver::prove_range_async  ──────► Nitro Enclave       │
//!  │       │                                  (vsock / PCR-pinned)│
//!  │       ▼                                                      │
//!  │  prover_submitProof(Nitro { attestation, signature })        │
//!  └──────────────────────────────────────────────────────────────┘
//! ```
//!
//! Uses the generic `ProofWorker` from `world-chain-proof-worker` (introduced in the
//! sp1-worker PR) for the lease→prove→submit loop, and the real `world-chain-prover-service`
//! types for the RPC client and proof data types.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy_primitives::{B256, Bytes};
use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use reqwest::blocking::Client as BlockingClient;
use serde_json::{Value, json};
use tracing::{info, warn};
use world_chain_chainspec::WorldChainSpec;
use world_chain_proof_core::{
    range::WorldRangeHardforkConfig,
    witness::{BlobData, WorldRangeWitnessData, preimage_store::PreimageStore},
};
use world_chain_proof_nitro::{
    ExpectedPcrs, NitroRangeProofRequest,
};
use world_chain_proof_protocol::WorldHardforkConfig as ProtocolHardforkConfig;
use world_chain_proof_succinct_client_utils::witness::executor::{
    WitnessExecutor, get_inputs_for_pipeline,
};
use world_chain_proof_succinct_ethereum_client_utils::executor::ETHDAWitnessExecutor;
use world_chain_proof_succinct_host_utils::witness_generation::{
    OnlineBlobStore, PreimageWitnessCollector,
};
use world_chain_proof_worker::ProofJobBackend;
use world_chain_prover_service::{
    ProofBackend, ProofData, ProofRequest, RpcProverServiceClient,
};

// Only the host-side NitroProver compiles on Linux (requires tokio-vsock / AF_VSOCK).
#[cfg(target_os = "linux")]
use world_chain_proof_nitro::host::{EnclaveEndpoint, NitroProver};

// Re-export ProofWorker and ProofWorkerConfig from the generic worker crate.
use world_chain_proof_worker::{ProofWorker, ProofWorkerConfig};

// ──────────────────────────────────────────────────────────────────────────────────────
// NitroBackend — ProofJobBackend implementation for the Nitro TEE lane
// ──────────────────────────────────────────────────────────────────────────────────────

/// Configuration for [`NitroBackend`].
#[derive(Clone, Debug)]
struct NitroBackendConfig {
    block_interval: u64,
    rollup_config_hash: B256,
    online: OnlineConfig,
    #[cfg(target_os = "linux")]
    enclave_cid: u32,
    #[cfg(target_os = "linux")]
    enclave_port: u32,
    expected_pcrs: ExpectedPcrs,
    schedule: WorldRangeHardforkConfig,
}

/// [`ProofJobBackend`] for the [`ProofBackend::Nitro`] lane: builds witnesses over RPC and
/// proves them inside a Nitro Enclave.
struct NitroBackend {
    config: NitroBackendConfig,
    /// Handle to the async runtime so we can call async enclave methods from `prove`.
    rt_handle: tokio::runtime::Handle,
}

impl NitroBackend {
    fn new(config: NitroBackendConfig, rt_handle: tokio::runtime::Handle) -> Self {
        Self { config, rt_handle }
    }
}

impl ProofJobBackend for NitroBackend {
    fn lane(&self) -> ProofBackend {
        ProofBackend::Nitro
    }

    #[cfg(target_os = "linux")]
    fn prove(&self, request: &ProofRequest) -> anyhow::Result<ProofData> {
        let start_block = request
            .l2_block_number
            .checked_sub(self.config.block_interval)
            .ok_or_else(|| {
                anyhow!(
                    "l2_block_number {} is below block_interval {}",
                    request.l2_block_number,
                    self.config.block_interval
                )
            })?;

        let endpoint = EnclaveEndpoint::with_port(
            self.config.enclave_cid,
            self.config.enclave_port,
        );
        let prover = NitroProver::with_runtime(
            endpoint,
            self.config.expected_pcrs,
            self.rt_handle.clone(),
        );

        // Build witness on the blocking threadpool.
        let witness = build_witness(
            &self.config.online,
            start_block,
            request.l2_block_number,
            request.l1_head,
            self.config.schedule.clone(),
        )
        .context("witness generation failed")?;

        // Serialize and send to the Nitro Enclave.
        let nitro_request = NitroRangeProofRequest::from_witness_data(&witness, None)
            .context("witness serialize")?;

        let artifact = self
            .rt_handle
            .block_on(prover.prove_range_async(nitro_request))
            .context("nitro enclave proving failed")?;

        // Verify the attested output matches the claimed game root.
        if artifact.boot_info.l2PostRoot != request.root_claim {
            bail!(
                "enclave post root {:?} != claimed root {:?}",
                artifact.boot_info.l2PostRoot,
                request.root_claim
            );
        }
        if artifact.boot_info.l2BlockNumber != request.l2_block_number {
            bail!(
                "enclave block number {} != claimed {}",
                artifact.boot_info.l2BlockNumber,
                request.l2_block_number
            );
        }
        if artifact.boot_info.l1Head != request.l1_head {
            bail!(
                "enclave l1 head {:?} != claimed {:?}",
                artifact.boot_info.l1Head,
                request.l1_head
            );
        }
        if artifact.boot_info.rollupConfigHash != self.config.rollup_config_hash {
            bail!(
                "enclave rollup config hash {:?} != expected {:?}",
                artifact.boot_info.rollupConfigHash,
                self.config.rollup_config_hash
            );
        }

        info!(
            post_root = ?artifact.boot_info.l2PostRoot,
            block = artifact.boot_info.l2BlockNumber,
            l1_head = ?artifact.boot_info.l1Head,
            rollup_config_hash = ?artifact.boot_info.rollupConfigHash,
            "enclave attested range proof"
        );

        Ok(ProofData::Nitro {
            attestation: Bytes::from(artifact.attestation_doc),
            signature: Bytes::from(artifact.signature),
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn prove(&self, _request: &ProofRequest) -> anyhow::Result<ProofData> {
        bail!("nitro-worker only supports Linux (requires AF_VSOCK and AWS Nitro Enclaves)")
    }
}

// ──────────────────────────────────────────────────────────────────────────────────────
// Witness generation (adapted from proofs/bin/src/main.rs)
// ──────────────────────────────────────────────────────────────────────────────────────

use kona_host::{DataFormat, single::SingleChainHost};
use kona_preimage::{BidirectionalChannel, HintWriter, NativeChannel, OracleReader};
use kona_proof::{CachingOracle, l1::OracleBlobProvider};

type DefaultOracleBase = CachingOracle<OracleReader<NativeChannel>, HintWriter<NativeChannel>>;
type WorldPreimageCollector = PreimageWitnessCollector<DefaultOracleBase>;
type WorldOnlineBlobStore = OnlineBlobStore<OracleBlobProvider<DefaultOracleBase>>;
type WorldEthWitnessExecutor = ETHDAWitnessExecutor<WorldPreimageCollector, WorldOnlineBlobStore>;

/// RPC-based configuration for online witness generation.
#[derive(Clone, Debug)]
struct OnlineConfig {
    l1_rpc: String,
    l1_beacon_rpc: String,
    l2_rpc: String,
    rollup_config_path: Option<PathBuf>,
    l2_chain_id: Option<u64>,
    witness_timeout: Duration,
}

/// Builds a `WorldRangeWitnessData` for the given block range using online RPC data.
///
/// This is synchronous and MUST NOT be called from within an async context.
/// Call it via `tokio::task::spawn_blocking` from async code.
fn build_witness(
    cfg: &OnlineConfig,
    start_block: u64,
    end_block: u64,
    l1_head: B256,
    schedule: WorldRangeHardforkConfig,
) -> Result<WorldRangeWitnessData> {
    let client = BlockingClient::new();

    let pre_root = l2_output_root(&client, &cfg.l2_rpc, start_block)?;
    let post_root = l2_output_root(&client, &cfg.l2_rpc, end_block)?;
    let pre_block_hash = l2_block_hash(&client, &cfg.l2_rpc, start_block)?;

    let host = SingleChainHost {
        l1_head,
        agreed_l2_head_hash: pre_block_hash,
        agreed_l2_output_root: pre_root,
        claimed_l2_output_root: post_root,
        claimed_l2_block_number: end_block,
        l2_node_address: Some(trim_rpc_url(&cfg.l2_rpc)),
        l1_node_address: Some(trim_rpc_url(&cfg.l1_rpc)),
        l1_beacon_address: Some(trim_rpc_url(&cfg.l1_beacon_rpc)),
        data_dir: None,
        data_format: DataFormat::default(),
        native: false,
        server: true,
        l2_chain_id: cfg.l2_chain_id,
        rollup_config_path: cfg.rollup_config_path.clone(),
        l1_config_path: None,
        enable_experimental_witness_endpoint: false,
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime for witness generation")?;

    rt.block_on(collect_witness_async(host, schedule, cfg.witness_timeout))
}

async fn collect_witness_async(
    host: SingleChainHost,
    schedule: WorldRangeHardforkConfig,
    timeout: Duration,
) -> Result<WorldRangeWitnessData> {
    let preimage = BidirectionalChannel::new()?;
    let hint = BidirectionalChannel::new()?;
    let mut server_task = host.start_server(hint.host, preimage.host).await?;

    let witness_fut = collect_from_channels(preimage.client, hint.client, schedule);
    tokio::pin!(witness_fut);

    let result = tokio::time::timeout(timeout, async {
        tokio::select! {
            r = &mut witness_fut => r,
            server = &mut server_task => match server {
                Ok(Ok(())) => Err(anyhow!("Kona server exited before witness completed")),
                Ok(Err(e)) => Err(anyhow!("Kona server failed: {e}")),
                Err(e) => Err(anyhow!("Kona server task panicked: {e}")),
            },
        }
    })
    .await
    .map_err(|_| anyhow!("witness generation timed out after {}s", timeout.as_secs()))??;

    if !server_task.is_finished() {
        server_task.abort();
    }
    Ok(result)
}

async fn collect_from_channels(
    preimage_chan: NativeChannel,
    hint_chan: NativeChannel,
    schedule: WorldRangeHardforkConfig,
) -> Result<WorldRangeWitnessData> {
    let preimage_witness_store = Arc::new(Mutex::new(PreimageStore::default()));
    let blob_data = Arc::new(Mutex::new(BlobData::default()));

    let preimage_oracle = Arc::new(CachingOracle::new(
        2048,
        OracleReader::new(preimage_chan),
        HintWriter::new(hint_chan),
    ));
    let blob_provider = OracleBlobProvider::new(preimage_oracle.clone());
    let oracle = Arc::new(PreimageWitnessCollector {
        preimage_oracle,
        preimage_witness_store: preimage_witness_store.clone(),
    });
    let beacon = OnlineBlobStore {
        provider: blob_provider,
        store: blob_data.clone(),
    };

    let executor = WorldEthWitnessExecutor::new();
    let (boot_info, input) = get_inputs_for_pipeline(oracle.clone())
        .await
        .map_err(|e| anyhow!("get_inputs_for_pipeline: {e:?}"))?;

    if let Some((cursor, l1_provider, l2_provider)) = input {
        let rollup_config = Arc::new(boot_info.rollup_config.clone());
        let l1_config = Arc::new(boot_info.l1_config.clone());
        let pipeline = executor
            .create_pipeline(
                rollup_config,
                l1_config,
                cursor.clone(),
                oracle,
                beacon,
                l1_provider,
                l2_provider.clone(),
            )
            .await
            .map_err(|e| anyhow!("create_pipeline: {e:?}"))?;
        executor
            .run_with_world_schedule(
                boot_info,
                pipeline,
                cursor,
                l2_provider,
                Some(schedule.clone()),
            )
            .await
            .map_err(|e| anyhow!("run_with_world_schedule: {e:?}"))?;
    }

    Ok(WorldRangeWitnessData::from_parts_with_world_config(
        preimage_witness_store.lock().unwrap().clone(),
        blob_data.lock().unwrap().clone(),
        schedule,
    ))
}

// ──────────────────────────────────────────────────────────────────────────────────────
// RPC helper utilities (adapted from proofs/bin/src/main.rs)
// ──────────────────────────────────────────────────────────────────────────────────────

fn trim_rpc_url(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

fn rpc_post(client: &BlockingClient, url: &str, body: Value) -> Result<Value> {
    let response = client
        .post(url)
        .json(&body)
        .send()
        .with_context(|| format!("HTTP POST to {url} failed"))?
        .json::<Value>()
        .context("failed to parse JSON response")?;
    if let Some(err) = response.get("error") {
        bail!("RPC error from {url}: {err}");
    }
    Ok(response["result"].clone())
}

fn l2_output_root(client: &BlockingClient, l2_rpc: &str, block: u64) -> Result<B256> {
    let result = rpc_post(
        client,
        l2_rpc,
        json!({
            "jsonrpc": "2.0",
            "method": "optimism_outputAtBlock",
            "params": [format!("0x{block:x}")],
            "id": 1
        }),
    )?;
    let root_str = result["outputRoot"]
        .as_str()
        .ok_or_else(|| anyhow!("missing outputRoot in optimism_outputAtBlock response"))?;
    root_str.parse().context("failed to parse output root")
}

fn l2_block_hash(client: &BlockingClient, l2_rpc: &str, block: u64) -> Result<B256> {
    let result = rpc_post(
        client,
        l2_rpc,
        json!({
            "jsonrpc": "2.0",
            "method": "eth_getBlockByNumber",
            "params": [format!("0x{block:x}"), false],
            "id": 1
        }),
    )?;
    let hash_str = result["hash"]
        .as_str()
        .ok_or_else(|| anyhow!("missing hash in eth_getBlockByNumber response"))?;
    hash_str.parse().context("failed to parse block hash")
}

// ──────────────────────────────────────────────────────────────────────────────────────
// CLI / config
// ──────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Network {
    #[value(name = "worldchain")]
    WorldChain,
    #[value(name = "worldchain-sepolia")]
    WorldChainSepolia,
}

impl Network {
    fn chain_id(self) -> u64 {
        match self {
            Self::WorldChain => 480,
            Self::WorldChainSepolia => 4801,
        }
    }

    fn chain_spec(self) -> Arc<WorldChainSpec> {
        match self {
            Self::WorldChain => WorldChainSpec::mainnet(),
            Self::WorldChainSepolia => WorldChainSpec::sepolia(),
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "nitro-worker",
    about = "World Chain Nitro TEE proving worker: leases jobs from the prover-service, \
             proves them in a Nitro Enclave, and submits the signed attestations back."
)]
struct Cli {
    /// prover-service JSON-RPC URL.
    #[arg(long, env = "PROVER_SERVICE_URL")]
    prover_service_url: String,

    /// World Chain L2 execution RPC URL.
    #[arg(long, env = "L2_RPC_URL")]
    l2_rpc: String,

    /// Ethereum L1 execution RPC URL.
    #[arg(long, env = "L1_RPC_URL")]
    l1_rpc: String,

    /// Ethereum L1 beacon API URL.
    #[arg(long, env = "L1_BEACON_RPC_URL")]
    l1_beacon_rpc: String,

    /// World Chain network.
    #[arg(long, env = "NETWORK", default_value = "worldchain")]
    network: Network,

    /// Rollup config JSON file. If omitted, uses the built-in network config.
    #[arg(long, env = "ROLLUP_CONFIG")]
    rollup_config: Option<PathBuf>,

    /// Rollup config hash override (required when --rollup-config is not supplied).
    #[arg(long, env = "ROLLUP_CONFIG_HASH")]
    rollup_config_hash: Option<B256>,

    /// L2 blocks between a proposal's parent and its claimed block (the proof system's
    /// `blockInterval` domain constant).
    #[arg(long, env = "BLOCK_INTERVAL")]
    block_interval: u64,

    /// vsock CID of the running Nitro Enclave.
    #[arg(long, env = "ENCLAVE_CID", default_value_t = 16)]
    enclave_cid: u32,

    /// vsock port the enclave listens on.
    #[arg(long, env = "ENCLAVE_PORT", default_value_t = world_chain_proof_nitro::protocol::DEFAULT_VSOCK_PORT)]
    enclave_port: u32,

    /// PCR0 hex (48 bytes). All three PCRs must be provided for production use.
    #[arg(long, env = "PCR0")]
    pcr0: Option<String>,

    /// PCR1 hex (48 bytes).
    #[arg(long, env = "PCR1")]
    pcr1: Option<String>,

    /// PCR2 hex (48 bytes).
    #[arg(long, env = "PCR2")]
    pcr2: Option<String>,

    /// Seconds to sleep between job-queue polls when no work is available.
    #[arg(long, env = "POLL_INTERVAL_SECONDS", default_value_t = 10)]
    poll_interval_seconds: u64,

    /// Maximum seconds to spend generating one Kona witness.
    #[arg(long, default_value_t = 900)]
    witness_timeout_seconds: u64,

    /// Maximum number of jobs proved concurrently. TEE attestation is cheaper than ZK
    /// proving, so this can be higher than for SP1 workers.
    #[arg(long, default_value_t = 1)]
    max_concurrent_jobs: usize,
}

// ──────────────────────────────────────────────────────────────────────────────────────
// Entry point
// ──────────────────────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let (schedule, rollup_config_hash) = proof_config(
        cli.network,
        cli.rollup_config.as_deref(),
        cli.rollup_config_hash,
    )?;
    let expected_pcrs =
        build_expected_pcrs(cli.pcr0.as_deref(), cli.pcr1.as_deref(), cli.pcr2.as_deref())?;

    let online_cfg = OnlineConfig {
        l1_rpc: cli.l1_rpc.clone(),
        l1_beacon_rpc: cli.l1_beacon_rpc.clone(),
        l2_rpc: cli.l2_rpc.clone(),
        rollup_config_path: cli.rollup_config.clone(),
        l2_chain_id: cli.rollup_config.is_none().then_some(cli.network.chain_id()),
        witness_timeout: Duration::from_secs(cli.witness_timeout_seconds),
    };

    info!(
        prover_service = %cli.prover_service_url,
        enclave_cid = cli.enclave_cid,
        block_interval = cli.block_interval,
        "nitro-worker starting"
    );

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("nitro-worker")
        .worker_threads(2)
        .max_blocking_threads(4)
        .build()
        .context("failed to build tokio runtime")?;

    let backend_config = NitroBackendConfig {
        block_interval: cli.block_interval,
        rollup_config_hash,
        online: online_cfg,
        #[cfg(target_os = "linux")]
        enclave_cid: cli.enclave_cid,
        #[cfg(target_os = "linux")]
        enclave_port: cli.enclave_port,
        expected_pcrs,
        schedule,
    };

    let backend = NitroBackend::new(backend_config, runtime.handle().clone());

    let queue = RpcProverServiceClient::new(&cli.prover_service_url)
        .with_context(|| format!("failed to connect to {}", cli.prover_service_url))?;

    let worker = ProofWorker::new(
        queue,
        backend,
        ProofWorkerConfig {
            poll_interval: Duration::from_secs(cli.poll_interval_seconds),
            max_concurrent_jobs: cli.max_concurrent_jobs,
        },
    );

    // Ctrl-C triggers a graceful shutdown: the worker stops leasing, flushes pending
    // reports, and resolves.
    let token = worker.cancellation_token();
    runtime.spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            info!("received ctrl-c, shutting down");
            token.cancel();
        }
    });

    runtime.block_on(worker);
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────────────
// Config helpers
// ──────────────────────────────────────────────────────────────────────────────────────

fn proof_config(
    network: Network,
    rollup_config_path: Option<&Path>,
    rollup_config_hash: Option<B256>,
) -> Result<(WorldRangeHardforkConfig, B256)> {
    use world_chain_proof_protocol::hash_rollup_config;

    if let Some(path) = rollup_config_path {
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let value: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        let protocol_cfg = ProtocolHardforkConfig::from_rollup_config_value(&value)
            .context("failed to parse rollup config hardfork schedule")?;
        let hash = hash_rollup_config(&value).context("failed to hash rollup config")?;
        return Ok((range_hardfork_config(&protocol_cfg), hash));
    }

    let hash = rollup_config_hash
        .context("provide --rollup-config or ROLLUP_CONFIG, or supply --rollup-config-hash")?;
    let spec = network.chain_spec();
    let protocol_cfg = ProtocolHardforkConfig::from_chain_spec(spec.as_ref());
    Ok((range_hardfork_config(&protocol_cfg), hash))
}

fn range_hardfork_config(config: &ProtocolHardforkConfig) -> WorldRangeHardforkConfig {
    WorldRangeHardforkConfig {
        bedrock_block: config.bedrock_block,
        regolith_time: config.regolith_time,
        canyon_time: config.canyon_time,
        ecotone_time: config.ecotone_time,
        fjord_time: config.fjord_time,
        granite_time: config.granite_time,
        holocene_time: config.holocene_time,
        isthmus_time: config.isthmus_time,
        jovian_time: config.jovian_time,
        tropo_time: config.tropo_time,
        strato_time: config.strato_time,
    }
}

fn build_expected_pcrs(
    pcr0: Option<&str>,
    pcr1: Option<&str>,
    pcr2: Option<&str>,
) -> Result<ExpectedPcrs> {
    match (pcr0, pcr1, pcr2) {
        (Some(p0), Some(p1), Some(p2)) => Ok(ExpectedPcrs {
            pcr0: hex_to_pcr(p0)?,
            pcr1: hex_to_pcr(p1)?,
            pcr2: hex_to_pcr(p2)?,
        }),
        (None, None, None) => {
            warn!(
                "PCRs not configured; using placeholder zeros. \
                 Production REQUIRES --pcr0/--pcr1/--pcr2."
            );
            Ok(ExpectedPcrs::PLACEHOLDER)
        }
        _ => bail!("provide all three of --pcr0/--pcr1/--pcr2, or none"),
    }
}

fn hex_to_pcr(s: &str) -> Result<[u8; world_chain_proof_nitro::PCR_LEN]> {
    let bytes = hex::decode(s.trim_start_matches("0x"))
        .with_context(|| format!("invalid PCR hex: {s}"))?;
    if bytes.len() != world_chain_proof_nitro::PCR_LEN {
        bail!(
            "PCR must be {} bytes, got {}",
            world_chain_proof_nitro::PCR_LEN,
            bytes.len()
        );
    }
    let mut arr = [0u8; world_chain_proof_nitro::PCR_LEN];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}
