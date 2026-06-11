//! `nitro-worker` binary: leases Nitro TEE proof jobs from the `prover-service`, proves
//! them inside a running Nitro Enclave, and submits the signed attestations back.
//!
//! # Architecture
//!
//! ```text
//!  ┌──────────────────────────────────────────────────────────────┐
//!  │                     nitro-worker                             │
//!  │                                                              │
//!  │  poll prover_getNextProof(Nitro)                             │
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
//! # prover-service compatibility
//!
//! The prover-service JSON-RPC API is defined on the `osiris/sp1-worker` branch in
//! `proofs/prover-service/`. Until that crate lands on `main`, the types and RPC
//! client are implemented inline here with types that are wire-compatible.
//! When `world-chain-prover-service` is merged, replace the inline definitions with
//! `use world_chain_prover_service::*`.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy_primitives::{Address, B256};
use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use reqwest::blocking::Client as BlockingClient;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::time::sleep;
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

// Only the host-side NitroProver compiles on Linux (requires tokio-vsock / AF_VSOCK).
#[cfg(target_os = "linux")]
use world_chain_proof_nitro::host::{EnclaveEndpoint, NitroProver};

// ──────────────────────────────────────────────────────────────────────────────────────
// Inline prover-service types
//
// Wire-compatible with `world-chain-prover-service` on `osiris/sp1-worker`.
// Replace with workspace crate imports once that PR is merged.
// ──────────────────────────────────────────────────────────────────────────────────────

/// Proof backend discriminant — must match `ProofBackend` in `world-chain-prover-service`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProofBackend {
    Sp1,
    Nitro,
}

impl std::fmt::Display for ProofBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sp1 => write!(f, "sp1"),
            Self::Nitro => write!(f, "nitro"),
        }
    }
}

/// Proof job handed to a worker by the prover-service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ProofRequest {
    backend: ProofBackend,
    game: Address,
    root_claim: B256,
    l2_block_number: u64,
    l1_head: B256,
}

impl ProofRequest {
    fn id(&self) -> ProofRequestId {
        use alloy_primitives::keccak256;
        let mut buf = Vec::with_capacity(1 + 20 + 32 + 8 + 32);
        buf.push(self.backend as u8);
        buf.extend_from_slice(self.game.as_slice());
        buf.extend_from_slice(self.root_claim.as_slice());
        buf.extend_from_slice(&self.l2_block_number.to_be_bytes());
        buf.extend_from_slice(self.l1_head.as_slice());
        ProofRequestId(keccak256(buf))
    }
}

/// Deterministic proof request identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct ProofRequestId(B256);

impl std::fmt::Display for ProofRequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Proof payload returned by the worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
enum ProofData {
    Nitro {
        /// Hex-encoded raw COSE_Sign1 attestation document (0x-prefixed).
        attestation: String,
        /// Hex-encoded 65-byte recoverable secp256k1 signature (0x-prefixed).
        signature: String,
    },
}

/// Proof response submitted to the prover-service.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProofResponse {
    id: ProofRequestId,
    proof: ProofData,
}

// ──────────────────────────────────────────────────────────────────────────────────────
// Inline prover-service JSON-RPC client
// ──────────────────────────────────────────────────────────────────────────────────────

/// Minimal JSON-RPC client for the `prover-service`.
///
/// Implements the three worker-facing methods:
/// - `prover_getNextProof(backend)` — lease the next queued job
/// - `prover_submitProof(proof)` — submit a successfully generated proof
/// - `prover_failProof(proof_id, reason)` — report a permanent failure
#[derive(Debug, Clone)]
struct ProverServiceClient {
    client: jsonrpsee::http_client::HttpClient,
}

impl ProverServiceClient {
    fn new(url: &str) -> Result<Self> {
        use jsonrpsee::http_client::HttpClientBuilder;
        let client = HttpClientBuilder::default()
            .build(url)
            .with_context(|| format!("failed to connect to prover-service at {url}"))?;
        Ok(Self { client })
    }

    async fn get_next_proof(&self, backend: ProofBackend) -> Result<Option<ProofRequest>> {
        use jsonrpsee::core::client::ClientT;
        let result: Option<ProofRequest> = self
            .client
            .request("prover_getNextProof", jsonrpsee::rpc_params![backend])
            .await
            .context("prover_getNextProof RPC failed")?;
        Ok(result)
    }

    async fn submit_proof(&self, proof: ProofResponse) -> Result<()> {
        use jsonrpsee::core::client::ClientT;
        let _: serde_json::Value = self
            .client
            .request("prover_submitProof", jsonrpsee::rpc_params![proof])
            .await
            .context("prover_submitProof RPC failed")?;
        Ok(())
    }

    async fn fail_proof(&self, proof_id: ProofRequestId, reason: String) -> Result<()> {
        use jsonrpsee::core::client::ClientT;
        let _: serde_json::Value = self
            .client
            .request("prover_failProof", jsonrpsee::rpc_params![proof_id, reason])
            .await
            .context("prover_failProof RPC failed")?;
        Ok(())
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

    let (schedule, _rollup_config_hash) = proof_config(
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

    runtime.block_on(worker_loop(cli, online_cfg, schedule, expected_pcrs))
}

async fn worker_loop(
    cli: Cli,
    online_cfg: OnlineConfig,
    schedule: WorldRangeHardforkConfig,
    expected_pcrs: ExpectedPcrs,
) -> Result<()> {
    let queue = ProverServiceClient::new(&cli.prover_service_url)?;
    let poll_interval = Duration::from_secs(cli.poll_interval_seconds);

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            info!("received ctrl-c, shutting down");
            let _ = shutdown_tx.send(());
        }
    });

    loop {
        if shutdown_rx.try_recv().is_ok() {
            info!("nitro-worker shut down cleanly");
            return Ok(());
        }

        let job = match queue.get_next_proof(ProofBackend::Nitro).await {
            Ok(Some(job)) => job,
            Ok(None) => {
                sleep(poll_interval).await;
                continue;
            }
            Err(err) => {
                warn!(%err, "failed to lease next proof job; retrying");
                sleep(poll_interval).await;
                continue;
            }
        };

        let id = job.id();
        info!(
            %id,
            game = %job.game,
            l2_block_number = job.l2_block_number,
            "processing nitro proof request"
        );

        let result = prove_job(&job, &online_cfg, &cli, &expected_pcrs, &schedule).await;

        match result {
            Ok(proof_data) => {
                let response = ProofResponse {
                    id,
                    proof: proof_data,
                };
                match queue.submit_proof(response).await {
                    Ok(()) => info!(%id, "proof submitted"),
                    Err(err) => warn!(%id, %err, "failed to submit proof"),
                }
            }
            Err(err) => {
                let reason = format!("{err:#}");
                warn!(%id, %reason, "proving failed; reporting to prover-service");
                if let Err(report_err) = queue.fail_proof(id, reason).await {
                    warn!(%id, %report_err, "failed to report proving failure");
                }
            }
        }
    }
}

/// Proves a single job: builds the witness, calls the enclave, and returns the proof data.
#[cfg(target_os = "linux")]
async fn prove_job(
    job: &ProofRequest,
    online_cfg: &OnlineConfig,
    cli: &Cli,
    expected_pcrs: &ExpectedPcrs,
    schedule: &WorldRangeHardforkConfig,
) -> Result<ProofData> {
    let start_block =
        job.l2_block_number
            .checked_sub(cli.block_interval)
            .ok_or_else(|| {
                anyhow!(
                    "l2_block_number {} is below block_interval {}",
                    job.l2_block_number,
                    cli.block_interval
                )
            })?;

    // Build witness on the blocking threadpool.
    let cfg = online_cfg.clone();
    let sched = schedule.clone();
    let l1_head = job.l1_head;
    let end_block = job.l2_block_number;
    let witness = tokio::task::spawn_blocking(move || {
        build_witness(&cfg, start_block, end_block, l1_head, sched)
    })
    .await
    .context("witness generation task panicked")?
    .context("witness generation failed")?;

    // Serialize and send to the Nitro Enclave.
    let request =
        NitroRangeProofRequest::from_witness_data(&witness, None).context("witness serialize")?;

    let endpoint = EnclaveEndpoint::with_port(cli.enclave_cid, cli.enclave_port);
    let rt_handle = tokio::runtime::Handle::current();
    let prover = NitroProver::with_runtime(endpoint, *expected_pcrs, rt_handle);

    let artifact = prover
        .prove_range_async(request)
        .await
        .context("nitro enclave proving failed")?;

    // Sanity-check the attested output matches the claimed game root.
    if artifact.boot_info.l2PostRoot != job.root_claim {
        bail!(
            "enclave post root {:?} != claimed root {:?}",
            artifact.boot_info.l2PostRoot,
            job.root_claim
        );
    }
    if artifact.boot_info.l2BlockNumber != job.l2_block_number {
        bail!(
            "enclave block number {} != claimed {}",
            artifact.boot_info.l2BlockNumber,
            job.l2_block_number
        );
    }

    info!(
        post_root = ?artifact.boot_info.l2PostRoot,
        block = artifact.boot_info.l2BlockNumber,
        "enclave attested range proof"
    );

    Ok(ProofData::Nitro {
        attestation: format!("0x{}", hex::encode(&artifact.attestation_doc)),
        signature: format!("0x{}", hex::encode(&artifact.signature)),
    })
}

/// Stub for non-Linux targets (vsock / Nitro only available on Linux).
#[cfg(not(target_os = "linux"))]
async fn prove_job(
    _job: &ProofRequest,
    _online_cfg: &OnlineConfig,
    _cli: &Cli,
    _expected_pcrs: &ExpectedPcrs,
    _schedule: &WorldRangeHardforkConfig,
) -> Result<ProofData> {
    bail!("nitro-worker only supports Linux (requires AF_VSOCK and AWS Nitro Enclaves)")
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
