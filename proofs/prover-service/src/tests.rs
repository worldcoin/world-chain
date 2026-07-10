use crate::{
    GetNextProofRequest, GetProofSessionRequest, LockId, ProofBackend, ProofData, ProofJobQueue,
    ProofJobQueueError, ProofRequest, ProofRequestId, ProofRequester, ProofResponse, ProofStatus,
    ProverService, ProverServiceConfig, RecordProofSessionRequest, RpcProverServiceClient,
    SubmitProofRequest, SucceededProofResponse, start_rpc_server,
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
        status_poller_interval: Duration::from_millis(25),
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

fn proof_for(req: &ProofRequest) -> SucceededProofResponse {
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
    SucceededProofResponse {
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

fn get_next_proof_request(backend: ProofBackend) -> GetNextProofRequest {
    GetNextProofRequest {
        backend,
        worker_id: worker_id(),
    }
}

fn submit_proof_request(proof: SucceededProofResponse, lock_id: LockId) -> SubmitProofRequest {
    SubmitProofRequest {
        proof,
        worker_id: worker_id(),
        lock_id,
    }
}

fn get_proof_session_request(
    proof_id: ProofRequestId,
    session_type: SessionType,
) -> GetProofSessionRequest {
    GetProofSessionRequest {
        proof_id,
        session_type,
    }
}

fn record_proof_session_request(
    proof_id: ProofRequestId,
    session_type: SessionType,
    lock_id: LockId,
    backend_session_id: String,
    status: BackendSessionStatus,
    failure_reason: Option<String>,
) -> RecordProofSessionRequest {
    RecordProofSessionRequest {
        proof_id,
        session_type,
        worker_id: worker_id(),
        lock_id,
        backend_session_id,
        status,
        failure_reason,
    }
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
        service.get_proof(id).await.unwrap(),
        ProofResponse::Pending(response)
            if response.id == id && response.status == ProofStatus::Created
    ));

    let locked = service
        .get_next_proof(get_next_proof_request(ProofBackend::Nitro))
        .await
        .unwrap()
        .locked_request
        .expect("job available");
    assert_eq!(locked.request, req);
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::Running
    );
    assert!(matches!(
        service.get_proof(id).await.unwrap(),
        ProofResponse::Pending(response)
            if response.id == id && response.status == ProofStatus::Running
    ));

    let response = proof_for(&req);
    service
        .submit_proof(submit_proof_request(response.clone(), locked.lock_id))
        .await
        .unwrap();
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::Succeeded
    );
    assert_eq!(
        service.get_proof(id).await.unwrap(),
        ProofResponse::Succeeded(response)
    );
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
            .get_next_proof(get_next_proof_request(ProofBackend::Sp1))
            .await
            .unwrap()
            .locked_request
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
        .get_next_proof(get_next_proof_request(ProofBackend::Sp1))
        .await
        .unwrap()
        .locked_request
        .expect("first lock");

    // Let the first lock expire so the job can be reclaimed.
    tokio::time::sleep(Duration::from_millis(80)).await;
    let second = service
        .get_next_proof(get_next_proof_request(ProofBackend::Sp1))
        .await
        .unwrap()
        .locked_request
        .expect("second lock");
    assert_ne!(first.lock_id, second.lock_id);

    // The first (now superseded) lock can no longer submit.
    assert!(matches!(
        service
            .submit_proof(submit_proof_request(proof_for(&req), first.lock_id))
            .await,
        Err(ProofJobQueueError::StaleLock(_))
    ));

    // The current lock owner can submit successfully.
    service
        .submit_proof(submit_proof_request(proof_for(&req), second.lock_id))
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
        .get_next_proof(get_next_proof_request(ProofBackend::Sp1))
        .await
        .unwrap()
        .locked_request
        .expect("lock");

    let mismatched = SucceededProofResponse {
        id,
        proof: ProofData::Nitro {
            attestation: Bytes::from(vec![0xcc]),
            signature: Bytes::from(vec![0xdd]),
        },
    };
    assert!(matches!(
        service
            .submit_proof(submit_proof_request(mismatched, locked.lock_id))
            .await,
        Err(ProofJobQueueError::BackendMismatch(_))
    ));
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::Running
    );
}

#[tokio::test]
async fn status_poller_marks_expired_exhausted_jobs_failed() {
    let config = ProverServiceConfig {
        lock_timeout: Duration::from_millis(50),
        ..test_config()
    };
    let max_attempts = config.max_attempts;
    let expected_failure_reason = format!("proof request exhausted max attempts ({max_attempts})");
    let Some(ctx) = service(config).await else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Sp1, 5);
    let id = service.request_proof(req).await.unwrap();

    let first = service
        .get_next_proof(get_next_proof_request(ProofBackend::Sp1))
        .await
        .unwrap()
        .locked_request
        .expect("first lock");
    tokio::time::sleep(Duration::from_millis(80)).await;

    let second = service
        .get_next_proof(get_next_proof_request(ProofBackend::Sp1))
        .await
        .unwrap()
        .locked_request
        .expect("second lock");
    assert_ne!(first.lock_id, second.lock_id);

    service
        .record_proof_session(record_proof_session_request(
            id,
            SessionType::Stark,
            second.lock_id,
            backend_session_id(5),
            BackendSessionStatus::Running,
            None,
        ))
        .await
        .unwrap();

    // we dont fail active work
    assert_eq!(
        service
            .mark_exhausted_proof_requests_failed()
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::Running
    );

    tokio::time::sleep(Duration::from_millis(80)).await;

    // without status poller, this job is not claimable
    assert!(
        service
            .get_next_proof(get_next_proof_request(ProofBackend::Sp1))
            .await
            .unwrap()
            .locked_request
            .is_none()
    );

    // clean up status poller status and expect to clean up 1 row
    assert_eq!(
        service
            .mark_exhausted_proof_requests_failed()
            .await
            .unwrap(),
        1
    );
    assert_eq!(service.proof_status(id).await.unwrap(), ProofStatus::Failed);

    assert!(matches!(
        service.get_proof(id).await.unwrap(),
        ProofResponse::Failed(response)
            if response.id == id && response.reason == expected_failure_reason
    ));

    // sp1 proof session is cleaned up too
    assert!(
        service
            .get_proof_session(get_proof_session_request(id, SessionType::Stark))
            .await
            .unwrap()
            .session
            .is_none()
    );
    assert_eq!(
        service
            .mark_exhausted_proof_requests_failed()
            .await
            .unwrap(),
        0
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
        .get_next_proof(get_next_proof_request(ProofBackend::Sp1))
        .await
        .unwrap()
        .locked_request
        .expect("lock");

    assert!(
        service
            .get_proof_session(get_proof_session_request(id, SessionType::Stark))
            .await
            .unwrap()
            .session
            .is_none()
    );

    service
        .record_proof_session(record_proof_session_request(
            id,
            SessionType::Stark,
            locked.lock_id,
            backend_session_id(1),
            BackendSessionStatus::Running,
            None,
        ))
        .await
        .unwrap();

    let session = service
        .get_proof_session(get_proof_session_request(id, SessionType::Stark))
        .await
        .unwrap()
        .session
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
        .get_next_proof(get_next_proof_request(ProofBackend::Sp1))
        .await
        .unwrap()
        .locked_request
        .expect("job available");
    assert_eq!(locked.request, req);

    let response = proof_for(&req);
    client
        .submit_proof(submit_proof_request(response.clone(), locked.lock_id))
        .await
        .unwrap();
    assert_eq!(
        client.get_proof(id).await.unwrap(),
        ProofResponse::Succeeded(response)
    );

    handle.stop().unwrap();
    handle.stopped().await;
}
