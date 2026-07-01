use crate::{
    ProofBackend, ProofData, ProofJobQueue, ProofJobQueueError, ProofRequest, ProofRequestError,
    ProofRequester, ProofResponse, ProofStatus, ProverService, ProverServiceConfig,
    RpcProverServiceClient, start_rpc_server,
    types::{BackendSessionStatus, SessionType},
};
use alloy_primitives::{Address, B256, Bytes};
use std::{sync::Arc, time::Duration};
use testcontainers::{ContainerAsync, runners::AsyncRunner};
use testcontainers_modules::postgres;

fn test_config() -> ProverServiceConfig {
    ProverServiceConfig {
        lock_timeout: Duration::from_secs(60),
        max_attempts: 2,
        max_retries: 2,
        backend_poll_interval: Duration::from_millis(25),
    }
}

struct TestService {
    service: ProverService,
    _postgres: ContainerAsync<postgres::Postgres>,
}

async fn service(config: ProverServiceConfig) -> Option<TestService> {
    let postgres = match postgres::Postgres::default().start().await {
        Ok(postgres) => postgres,
        Err(error) => {
            eprintln!("skipping postgres-backed prover-service test: {error}");
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
    let service = ProverService::connect(&database_url, config)
        .await
        .expect("postgres-backed service");
    Some(TestService {
        service,
        _postgres: postgres,
    })
}

fn request(backend: ProofBackend, seed: u8) -> ProofRequest {
    ProofRequest {
        backend,
        game: Address::with_last_byte(seed),
        root_claim: B256::with_last_byte(seed),
        l2_block_number: u64::from(seed) * 100,
        l1_head: B256::with_last_byte(seed.wrapping_add(1)),
    }
}

fn proof_for(req: &ProofRequest) -> ProofResponse {
    let proof = match req.backend {
        ProofBackend::Sp1 => ProofData::Sp1 {
            proof: Bytes::from(vec![0xaa]),
            public_values: Bytes::from(vec![0xbb]),
        },
        ProofBackend::Nitro => ProofData::Nitro {
            attestation: Bytes::from(vec![0xcc]),
            signature: Bytes::from(vec![0xdd]),
        },
    };
    ProofResponse {
        id: req.id(),
        proof,
    }
}

fn backend_session_id(seed: u8) -> String {
    format!("backend-session-{seed}")
}

fn worker_id() -> String {
    "test-worker".to_string()
}

#[test]
fn request_id_is_deterministic() {
    let sp1 = request(ProofBackend::Sp1, 1);
    assert_eq!(sp1.id(), request(ProofBackend::Sp1, 1).id());
    assert_ne!(sp1.id(), request(ProofBackend::Nitro, 1).id());
    assert_ne!(sp1.id(), request(ProofBackend::Sp1, 2).id());
}

#[tokio::test]
async fn full_lifecycle_succeeds() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Nitro, 1);

    let id = service.request_proof(req.clone()).await.unwrap();
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::Created
    );
    assert!(matches!(
        service.get_proof(id).await,
        Err(ProofRequestError::Pending {
            status: ProofStatus::Created,
            ..
        })
    ));

    let locked = service
        .get_next_proof(ProofBackend::Nitro, worker_id())
        .await
        .unwrap()
        .expect("job available");
    assert_eq!(locked.request, req);
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::Running
    );
    assert!(matches!(
        service.get_proof(id).await,
        Err(ProofRequestError::Pending {
            status: ProofStatus::Running,
            ..
        })
    ));

    let response = proof_for(&req);
    service
        .submit_proof(response.clone(), worker_id(), locked.lock_id)
        .await
        .unwrap();
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::Succeeded
    );
    assert_eq!(service.get_proof(id).await.unwrap(), response);
}

#[tokio::test]
async fn duplicate_request_is_deduplicated() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Sp1, 2);

    let first = service.request_proof(req.clone()).await.unwrap();
    let second = service.request_proof(req.clone()).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(first, req.id());
    assert_eq!(
        service.proof_status(first).await.unwrap(),
        ProofStatus::Created
    );
}

#[tokio::test]
async fn get_next_proof_on_empty_queue_returns_none() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = ctx.service;
    assert!(
        service
            .get_next_proof(ProofBackend::Sp1, worker_id())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn stale_lock_is_rejected_and_reclaim_succeeds() {
    let Some(ctx) = service(ProverServiceConfig {
        lock_timeout: Duration::from_millis(50),
        ..test_config()
    })
    .await
    else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Sp1, 3);
    let id = service.request_proof(req.clone()).await.unwrap();

    let first = service
        .get_next_proof(ProofBackend::Sp1, worker_id())
        .await
        .unwrap()
        .expect("first lock");

    // Let the first lock expire so the job can be reclaimed.
    tokio::time::sleep(Duration::from_millis(80)).await;
    let second = service
        .get_next_proof(ProofBackend::Sp1, worker_id())
        .await
        .unwrap()
        .expect("second lock");
    assert_ne!(first.lock_id, second.lock_id);

    // The first (now superseded) lock can no longer submit.
    assert!(matches!(
        service
            .submit_proof(proof_for(&req), worker_id(), first.lock_id)
            .await,
        Err(ProofJobQueueError::StaleLock(_))
    ));

    // The current lock owner can submit successfully.
    service
        .submit_proof(proof_for(&req), worker_id(), second.lock_id)
        .await
        .unwrap();
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::Succeeded
    );
}

#[tokio::test]
async fn submit_proof_with_wrong_backend_is_rejected() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Sp1, 4);
    let id = service.request_proof(req.clone()).await.unwrap();
    let locked = service
        .get_next_proof(ProofBackend::Sp1, worker_id())
        .await
        .unwrap()
        .expect("lock");

    let mismatched = ProofResponse {
        id,
        proof: ProofData::Nitro {
            attestation: Bytes::from(vec![0xcc]),
            signature: Bytes::from(vec![0xdd]),
        },
    };
    assert!(matches!(
        service
            .submit_proof(mismatched, worker_id(), locked.lock_id)
            .await,
        Err(ProofJobQueueError::BackendMismatch { .. })
    ));
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::Running
    );
}

#[tokio::test]
async fn record_and_get_proof_session_round_trips() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Sp1, 5);
    let id = service.request_proof(req.clone()).await.unwrap();
    let locked = service
        .get_next_proof(ProofBackend::Sp1, worker_id())
        .await
        .unwrap()
        .expect("lock");

    assert!(
        service
            .get_proof_session(id, SessionType::Stark)
            .await
            .unwrap()
            .is_none()
    );

    service
        .record_proof_session(
            id,
            SessionType::Stark,
            worker_id(),
            locked.lock_id,
            backend_session_id(1),
            BackendSessionStatus::Running,
        )
        .await
        .unwrap();

    let session = service
        .get_proof_session(id, SessionType::Stark)
        .await
        .unwrap()
        .expect("session recorded");
    assert_eq!(session.backend_session_id, backend_session_id(1));
    assert_eq!(session.status, BackendSessionStatus::Running);
}

#[tokio::test]
async fn rpc_end_to_end() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = Arc::new(ctx.service);
    let (addr, handle) = start_rpc_server("127.0.0.1:0".parse().unwrap(), service)
        .await
        .unwrap();
    let client = RpcProverServiceClient::new(format!("http://{addr}")).unwrap();

    let req = request(ProofBackend::Sp1, 6);
    let id = client.request_proof(req.clone()).await.unwrap();
    assert_eq!(client.proof_status(id).await.unwrap(), ProofStatus::Created);

    let locked = client
        .get_next_proof(ProofBackend::Sp1, worker_id())
        .await
        .unwrap()
        .expect("job available");
    assert_eq!(locked.request, req);

    let response = proof_for(&req);
    client
        .submit_proof(response.clone(), worker_id(), locked.lock_id)
        .await
        .unwrap();
    assert_eq!(client.get_proof(id).await.unwrap(), response);

    handle.stop().unwrap();
    handle.stopped().await;
}
