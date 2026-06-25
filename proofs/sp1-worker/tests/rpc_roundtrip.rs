//! End-to-end test: a real in-process `prover-service` with the worker attached over JSON-RPC.

use std::{sync::Arc, time::Duration};

use alloy_primitives::{Address, B256, Bytes};
use testcontainers::{ContainerAsync, runners::AsyncRunner};
use testcontainers_modules::postgres;
use world_chain_prover_service::{
    BackendProofState, BackendUpdate, ProofBackend, ProofData, ProofRequest, ProofRequester,
    ProofStatus, ProverService, ProverServiceConfig, RpcProverServiceClient, start_rpc_server,
};
use world_chain_sp1_worker::{ProofJobBackend, ProofWorker, ProofWorkerConfig};

/// Backend returning a canned SP1 proof for any request, without RPC or a prover.
struct MockBackend;

impl ProofJobBackend for MockBackend {
    fn lane(&self) -> ProofBackend {
        ProofBackend::Sp1
    }

    fn start(&self, _request: &ProofRequest) -> anyhow::Result<BackendUpdate> {
        Ok(BackendUpdate::Complete(ProofData::Sp1 {
            proof: Bytes::from_static(&[0xaa, 0xbb]),
            public_values: Bytes::from_static(&[0x01]),
        }))
    }

    fn advance(
        &self,
        _request: &ProofRequest,
        _state: BackendProofState,
    ) -> anyhow::Result<BackendUpdate> {
        anyhow::bail!("mock backend has no durable backend jobs")
    }
}

async fn postgres_service() -> Option<(Arc<ProverService>, ContainerAsync<postgres::Postgres>)> {
    let postgres = match postgres::Postgres::default().start().await {
        Ok(postgres) => postgres,
        Err(error) => {
            eprintln!("skipping postgres-backed sp1-worker RPC test: {error}");
            return None;
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
            .expect("postgres-backed service"),
    );
    Some((service, postgres))
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_completes_requested_proof_over_rpc() {
    let Some((service, _postgres)) = postgres_service().await else {
        return;
    };
    let (addr, server) = start_rpc_server("127.0.0.1:0".parse().expect("addr"), service)
        .await
        .expect("rpc server starts");
    let url = format!("http://{addr}");

    let requester = RpcProverServiceClient::new(&url).expect("client connects");
    let queue = RpcProverServiceClient::new(&url).expect("client connects");

    let request = ProofRequest {
        backend: ProofBackend::Sp1,
        game: Address::repeat_byte(0x42),
        root_claim: B256::repeat_byte(0x07),
        l2_block_number: 1_200,
        l1_head: B256::repeat_byte(0x11),
    };
    let id = requester
        .request_proof(request.clone())
        .await
        .expect("request accepted");
    assert_eq!(id, request.id());

    let worker = ProofWorker::new(
        queue,
        MockBackend,
        ProofWorkerConfig {
            worker_id: "test-worker".to_string(),
            poll_interval: Duration::from_millis(10),
            max_concurrent_jobs: 1,
        },
    );
    let worker_handle = tokio::spawn(worker);

    let mut status = ProofStatus::Queued;
    for _ in 0..200 {
        status = requester.proof_status(id).await.expect("status known");
        if status == ProofStatus::Completed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(status, ProofStatus::Completed);

    let response = requester.get_proof(id).await.expect("proof available");
    assert_eq!(response.id, id);
    let ProofData::Sp1 {
        proof,
        public_values,
    } = response.proof
    else {
        panic!("expected SP1 proof data");
    };
    assert_eq!(proof.as_ref(), [0xaa, 0xbb]);
    assert!(!public_values.is_empty());

    worker_handle.abort();
    drop(server);
}
