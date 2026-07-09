//! Full end-to-end proving test for the SP1 worker.
//!
//! Runs the real worker against a real `prover-service` and a real [`CpuSuccinctProver`],
//! generating a witness from live RPC endpoints and proving it with the local CPU prover. This
//! is the highest-fidelity test of the worker — it exercises lease → witness → prove →
//! submit end to end — but it needs external infrastructure, so it is `#[ignore]`d and only
//! runs when the required environment is present.
//!
//! # Running
//!
//! Point it at a chain with derivable data (the devnet's exposed RPCs, or staging). With the
//! devnet up (`xtask devnet up`):
//!
//! ```bash
//! export L1_RPC_URL=http://127.0.0.1:8545
//! export L2_RPC_URL=http://127.0.0.1:9545
//! export L1_BEACON_RPC_URL=$L1_RPC_URL          # devnet uses calldata DA; L1 RPC doubles as beacon
//! export ROLLUP_RPC_URL=http://127.0.0.1:7545   # op-node, for output roots
//! export ROLLUP_CONFIG=/path/to/rollup.json
//! export SP1_PROVER=cpu                          # cpu | mock | network
//! export SP1_PRIVATE_KEY=<your key>              # required for SP1_PROVER=network
//! cargo test -p world-chain-sp1-worker --test e2e_proving -- --ignored --nocapture
//! ```
//!
//! The SP1 guest ELFs are baked into the worker at compile time via
//! `sp1_sdk::include_elf!()` (see `proofs/succinct/elfs/build.rs`); no path-based
//! overrides are required.

use std::{path::PathBuf, sync::Arc, time::Duration};

use alloy_primitives::{Address, B256};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres;
use world_chain_proof_kona_host_utils::online::{OnlineHostConfig, resolve_l1_head};
use world_chain_proof_succinct_host_utils::{
    Sp1ProverKind, WorldSuccinctProver,
    cpu_prover::{CpuSuccinctProver, SP1ProofMode},
    mock_prover::MockSuccinctProver,
    network_prover::NetworkSuccinctProver,
};
use world_chain_proof_worker::WorkerHeartbeatConfig;
use world_chain_proofs::{ConsensusProvider, OptimismConsensusClient};
use world_chain_prover_service::{
    ProofBackend, ProofData, ProofRequest, ProofRequester, ProofResponse, ProofStatus,
    ProverService, ProverServiceConfig, RpcProverServiceClient, start_rpc_server,
};
use world_chain_sp1_worker::{
    ProofWorker, ProofWorkerConfig, RetryConfig, Sp1Backend, Sp1BackendConfig,
};

/// Reads a required env var, or returns `None` (with a skip message) when absent.
fn required(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Some(value),
        _ => {
            eprintln!("skipping e2e_proving: {name} is not set");
            None
        }
    }
}

fn prover_kind() -> Sp1ProverKind {
    std::env::var("SP1_PROVER")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(Sp1ProverKind::Cpu)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs live L1/L2/beacon RPCs and SP1 ELFs; run explicitly with --ignored"]
async fn worker_proves_real_range_end_to_end() {
    let (Some(l1_rpc), Some(l2_rpc), Some(l1_beacon_rpc), Some(rollup_rpc), Some(rollup_config)) = (
        required("L1_RPC_URL"),
        required("L2_RPC_URL"),
        required("L1_BEACON_RPC_URL"),
        required("ROLLUP_RPC_URL"),
        required("ROLLUP_CONFIG"),
    ) else {
        return;
    };

    let block_interval: u64 = std::env::var("E2E_BLOCK_INTERVAL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let split_count: u64 = std::env::var("E2E_SPLIT_COUNT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let timeout = Duration::from_secs(
        std::env::var("E2E_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3600),
    );

    // Choose a finalized claimed block and read its canonical output root: the worker must
    // reproduce exactly this root from the witness or it fails the job.
    let consensus = OptimismConsensusClient::new(rollup_rpc);
    let claimed_block = match std::env::var("E2E_L2_BLOCK")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        Some(block) => block,
        None => consensus
            .latest_l2_finalized_block()
            .await
            .expect("query finalized L2 head"),
    };
    assert!(
        claimed_block > block_interval,
        "claimed block {claimed_block} must exceed block_interval {block_interval}"
    );
    let root_claim = consensus
        .output_root_at_block(claimed_block)
        .await
        .expect("query claimed output root");

    let l1_head = resolve_l1_head(&reqwest::Client::new(), &l2_rpc, &l1_rpc, claimed_block)
        .await
        .expect("resolve l1 head");

    let rollup_config_value =
        serde_json::from_slice(&std::fs::read(&rollup_config).expect("read rollup config"))
            .expect("parse rollup config");
    let host = OnlineHostConfig::from_rollup_config_value(
        &rollup_config_value,
        l1_rpc,
        l1_beacon_rpc,
        l2_rpc,
        Some(PathBuf::from(rollup_config)),
        timeout,
    )
    .expect("build host config");

    let kind = prover_kind();
    match kind {
        Sp1ProverKind::Cpu => {
            let prover = CpuSuccinctProver::new(SP1ProofMode::Groth16)
                .await
                .expect("build prover");
            run_worker_proves_real_range_end_to_end_with_prover(
                host,
                prover,
                kind,
                block_interval,
                split_count,
                root_claim,
                l1_head,
                claimed_block,
                timeout,
            )
            .await;
        }
        Sp1ProverKind::Mock => {
            let prover = MockSuccinctProver::new(SP1ProofMode::Groth16)
                .await
                .expect("build prover");
            run_worker_proves_real_range_end_to_end_with_prover(
                host,
                prover,
                kind,
                block_interval,
                split_count,
                root_claim,
                l1_head,
                claimed_block,
                timeout,
            )
            .await;
        }
        Sp1ProverKind::Network => {
            let Some(private_key) = required("SP1_PRIVATE_KEY") else {
                return;
            };
            let prover = NetworkSuccinctProver::new(SP1ProofMode::Groth16, &private_key)
                .await
                .expect("build prover");
            run_worker_proves_real_range_end_to_end_with_prover(
                host,
                prover,
                kind,
                block_interval,
                split_count,
                root_claim,
                l1_head,
                claimed_block,
                timeout,
            )
            .await;
        }
    }
}

async fn run_worker_proves_real_range_end_to_end_with_prover<P>(
    host: OnlineHostConfig,
    prover: P,
    kind: Sp1ProverKind,
    block_interval: u64,
    split_count: u64,
    root_claim: B256,
    l1_head: B256,
    claimed_block: u64,
    timeout: Duration,
) where
    P: WorldSuccinctProver + Send + Sync + 'static,
{
    let backend = Sp1Backend::new(
        host,
        prover,
        Sp1BackendConfig {
            block_interval,
            split_count,
            prover_address: Address::ZERO,
            allow_unfinalized: false,
        },
    );

    // Real Postgres-backed prover-service over JSON-RPC, just like production.
    let postgres = match postgres::Postgres::default().start().await {
        Ok(postgres) => postgres,
        Err(error) => {
            eprintln!("skipping e2e_proving: failed to start postgres: {error}");
            return;
        }
    };
    let database_url = format!(
        "postgres://postgres:postgres@{}:{}/postgres",
        postgres.get_host().await.expect("postgres host"),
        postgres
            .get_host_port_ipv4(5432)
            .await
            .expect("postgres port")
    );
    let service = Arc::new(
        ProverService::connect(&database_url, ProverServiceConfig::default())
            .await
            .expect("config"),
    );
    let (addr, _server) = start_rpc_server("127.0.0.1:0".parse().unwrap(), service)
        .await
        .expect("start prover-service");
    let url = format!("http://{addr}");
    let client = RpcProverServiceClient::new(&url).expect("client");
    let worker = ProofWorker::new(
        RpcProverServiceClient::new(&url).expect("client"),
        backend,
        ProofWorkerConfig {
            worker_id: "test-worker".to_string(),
            poll_interval: Duration::from_millis(500),
            max_concurrent_jobs: 1,
            retry_config: RetryConfig::default(),
            heartbeat_config: WorkerHeartbeatConfig::default(),
        },
    );
    let token = worker.cancellation_token();
    let worker_handle = tokio::spawn(worker);

    let request = ProofRequest {
        backend: ProofBackend::Sp1,
        game: Address::repeat_byte(0x42),
        root_claim,
        l2_block_number: claimed_block,
        l1_head,
    };
    let id = client
        .request_proof(request)
        .await
        .expect("enqueue proof request");
    eprintln!(
        "enqueued {id} for block {claimed_block} (interval {block_interval}, {kind} prover); proving may take a while"
    );

    let deadline = tokio::time::Instant::now() + timeout;
    let status = loop {
        match client.proof_status(id).await.expect("poll status") {
            ProofStatus::Succeeded => break ProofStatus::Succeeded,
            ProofStatus::Failed => panic!("proof request failed: {:?}", client.get_proof(id).await),
            pending => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "proof did not complete within {timeout:?} (last status {pending})"
                );
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    };
    assert_eq!(status, ProofStatus::Succeeded);

    let response = client.get_proof(id).await.expect("fetch proof");
    let ProofResponse::Succeeded(response) = response else {
        panic!("expected succeeded proof response");
    };
    assert_eq!(response.id, id);
    let ProofData::Sp1 {
        proof,
        public_values,
    } = response.proof
    else {
        panic!("expected SP1 proof data");
    };
    assert!(!public_values.is_empty(), "public values must be populated");
    if !matches!(kind, Sp1ProverKind::Mock) {
        assert!(!proof.is_empty(), "non-mock proof must be non-empty");
    }

    token.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), worker_handle).await;
}
