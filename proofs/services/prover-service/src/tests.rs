use crate::{
    GetNextProofRequest, GetProofSessionRequest, LockId, ProofBackend, ProofData, ProofJobQueue,
    ProofJobQueueError, ProofRequest, ProofRequestError, ProofRequestId, ProofRequester,
    ProofResponse, ProofStatus, ProverService, ProverServiceConfig, RecordProofSessionRequest,
    RpcProverServiceClient, SubmitProofRequest, SucceededProofResponse, start_rpc_server,
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

async fn expire_proof_lock(service: &ProverService, id: ProofRequestId, lock_id: LockId) {
    let result = sqlx::query(
        r#"
        UPDATE proof_requests
        SET lock_expires_at = NOW() - INTERVAL '1 hour'
        WHERE proof_id = $1 AND lock_id = $2
        "#,
    )
    .bind(id.0.as_slice().to_vec())
    .bind(lock_id.0)
    .execute(service.pool())
    .await
    .expect("expire proof lock");

    assert_eq!(result.rows_affected(), 1);
}

fn request(backend: ProofBackend, seed: u8) -> ProofRequest {
    ProofRequest {
        backend,
        game: Address::with_last_byte(seed),
        root_claim: B256::with_last_byte(seed),
        l2_block_number: u64::from(seed) * 100,
        l1_head: B256::with_last_byte(seed.wrapping_add(1)),
        verifier_id: B256::with_last_byte(seed.wrapping_add(2)),
        range_vkey_commitment: (backend == ProofBackend::Sp1)
            .then(|| B256::with_last_byte(seed.wrapping_add(3))),
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
            public_values: Bytes::from(vec![0xee]),
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

#[tokio::test]
async fn cancellation_revokes_leases_preserves_completed_proofs_and_can_restart() {
    let ctx = service(test_config()).await.expect("Postgres is required");
    let service = &ctx.service;
    let running = request(ProofBackend::Sp1, 50);
    let pending = request(ProofBackend::Nitro, 50);
    let mut completed = request(ProofBackend::Nitro, 51);
    completed.game = running.game;
    let unrelated = request(ProofBackend::Sp1, 52);
    for req in [&running, &pending, &completed, &unrelated] {
        service.request_proof(req.clone()).await.unwrap();
    }
    let completed_lock = service
        .get_next_proof(get_next_proof_request_for(&completed))
        .await
        .unwrap()
        .locked_request
        .unwrap();
    service
        .submit_proof(submit_proof_request(
            proof_for(&completed),
            completed_lock.lock_id,
        ))
        .await
        .unwrap();
    let lock = service
        .get_next_proof(get_next_proof_request_for(&running))
        .await
        .unwrap()
        .locked_request
        .unwrap();
    service
        .record_proof_session(record_proof_session_request(
            running.id(),
            SessionType::Stark,
            lock.lock_id,
            backend_session_id(50),
            BackendSessionStatus::Running,
            None,
        ))
        .await
        .unwrap();

    assert_eq!(service.cancel_game_proofs(running.game).await.unwrap(), 2);
    assert_eq!(service.cancel_game_proofs(running.game).await.unwrap(), 0);
    for req in [&running, &pending] {
        assert_eq!(
            service.proof_status(req.id()).await.unwrap(),
            ProofStatus::Cancelled
        );
        assert!(
            service
                .get_next_proof(get_next_proof_request_for(req))
                .await
                .unwrap()
                .locked_request
                .is_none()
        );
        assert!(matches!(
            service.get_proof(req.id()).await.unwrap(),
            ProofResponse::Failed(_)
        ));
    }
    assert_eq!(
        service.get_proof(completed.id()).await.unwrap(),
        ProofResponse::Succeeded(proof_for(&completed))
    );
    assert_eq!(
        service.proof_status(unrelated.id()).await.unwrap(),
        ProofStatus::Created
    );
    assert!(matches!(
        service
            .heartbeat(crate::HeartbeatRequest {
                proof_id: running.id(),
                worker_id: worker_id(),
                lock_id: lock.lock_id,
            })
            .await,
        Err(ProofJobQueueError::AlreadyTerminal(_))
    ));
    assert!(
        service
            .submit_proof(submit_proof_request(proof_for(&running), lock.lock_id))
            .await
            .is_err()
    );
    assert!(
        service
            .record_proof_session(record_proof_session_request(
                running.id(),
                SessionType::Stark,
                lock.lock_id,
                backend_session_id(50),
                BackendSessionStatus::Completed,
                None,
            ))
            .await
            .is_err()
    );
    let session_status: String =
        sqlx::query_scalar("SELECT status FROM proof_sessions WHERE proof_id = $1")
            .bind(running.id().0.as_slice())
            .fetch_one(service.pool())
            .await
            .unwrap();
    assert_eq!(session_status, "RUNNING");

    service.request_proof(running.clone()).await.unwrap();
    let retries: i32 =
        sqlx::query_scalar("SELECT retry_count FROM proof_requests WHERE proof_id = $1")
            .bind(running.id().0.as_slice())
            .fetch_one(service.pool())
            .await
            .unwrap();
    assert_eq!(retries, 0);
    let restarted = service
        .get_next_proof(get_next_proof_request_for(&running))
        .await
        .unwrap()
        .locked_request
        .unwrap();
    assert_ne!(lock.lock_id, restarted.lock_id);
    let resumed_session = service
        .get_proof_session(get_proof_session_request(running.id(), SessionType::Stark))
        .await
        .unwrap()
        .session
        .unwrap();
    assert_eq!(resumed_session.backend_session_id, backend_session_id(50));
    assert_eq!(resumed_session.status, BackendSessionStatus::Running);
    assert!(
        service
            .submit_proof(submit_proof_request(proof_for(&running), lock.lock_id))
            .await
            .is_err()
    );
    service
        .submit_proof(submit_proof_request(proof_for(&running), restarted.lock_id))
        .await
        .unwrap();
}

#[tokio::test]
async fn cleanup_retains_needed_jobs_and_cancels_after_council_support() {
    use crate::status_poller::{cancel_obsolete_proofs, tests::push_game_state};
    use alloy_provider::ProviderBuilder;
    use alloy_transport::mock::Asserter;

    let ctx = service(test_config()).await.expect("Postgres is required");
    let service = &ctx.service;
    let req = request(ProofBackend::Sp1, 60);
    service.request_proof(req.clone()).await.unwrap();
    let asserter = Asserter::new();
    let provider = ProviderBuilder::new().connect_mocked_client(asserter.clone());

    asserter.push_failure_msg("L1 unavailable");
    cancel_obsolete_proofs(service, &provider).await.unwrap();
    assert_eq!(
        service.proof_status(req.id()).await.unwrap(),
        ProofStatus::Created
    );
    push_game_state(&asserter, 0, 1, 4);
    cancel_obsolete_proofs(service, &provider).await.unwrap();
    assert_eq!(
        service.proof_status(req.id()).await.unwrap(),
        ProofStatus::Created
    );
    push_game_state(&asserter, 0, 3, 6);
    cancel_obsolete_proofs(service, &provider).await.unwrap();
    assert_eq!(
        service.proof_status(req.id()).await.unwrap(),
        ProofStatus::Cancelled
    );
    assert!(service.active_games().await.unwrap().is_empty());
}

fn get_next_proof_request_for(request: &ProofRequest) -> GetNextProofRequest {
    GetNextProofRequest {
        backend: request.backend,
        worker_id: worker_id(),
        verifier_id: request.verifier_id,
        range_vkey_commitment: request.range_vkey_commitment,
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

    let mut different_l1_head = sp1.clone();
    different_l1_head.l1_head = B256::with_last_byte(0xff);
    assert_ne!(sp1.id(), different_l1_head.id());

    let mut different_verifier_values = sp1.clone();
    different_verifier_values.verifier_id = B256::with_last_byte(0xfe);
    different_verifier_values.range_vkey_commitment = Some(B256::with_last_byte(0xfd));
    assert_eq!(sp1.id(), different_verifier_values.id());
}

#[tokio::test]
async fn full_lifecycle_succeeds() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Nitro, 1);

    let response = service.request_proof(req.clone()).await.unwrap();
    let id = response.proof_id;
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
        .get_next_proof(get_next_proof_request_for(&req))
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
    assert_eq!(first.proof_id, req.id());
    assert_eq!(
        service.proof_status(first.proof_id).await.unwrap(),
        ProofStatus::Created
    );
}

#[tokio::test]
async fn worker_claims_only_matching_verifier_values() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = ctx.service;
    let old = request(ProofBackend::Sp1, 1);
    let new = request(ProofBackend::Sp1, 2);
    service.request_proof(old.clone()).await.unwrap();
    service.request_proof(new.clone()).await.unwrap();

    let locked = service
        .get_next_proof(get_next_proof_request_for(&new))
        .await
        .unwrap()
        .locked_request
        .expect("matching job");

    assert_eq!(locked.request, new);
}

#[tokio::test]
async fn sp1_range_vkey_commitment_must_match() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Sp1, 1);
    service.request_proof(req.clone()).await.unwrap();
    let mut worker = get_next_proof_request_for(&req);
    worker.range_vkey_commitment = Some(B256::with_last_byte(0xff));

    assert!(
        service
            .get_next_proof(worker)
            .await
            .unwrap()
            .locked_request
            .is_none()
    );
}

#[tokio::test]
async fn nitro_image_id_must_match() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Nitro, 1);
    service.request_proof(req.clone()).await.unwrap();
    let mut worker = get_next_proof_request_for(&req);
    worker.verifier_id = B256::with_last_byte(0xff);

    assert!(
        service
            .get_next_proof(worker)
            .await
            .unwrap()
            .locked_request
            .is_none()
    );
}

#[tokio::test]
async fn immutable_verifier_values_cannot_change_for_existing_request() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Sp1, 1);
    let id = service.request_proof(req.clone()).await.unwrap().proof_id;
    let mut mismatched = req;
    mismatched.verifier_id = B256::with_last_byte(0xff);

    assert!(matches!(
        service.request_proof(mismatched).await,
        Err(ProofRequestError::RequestMismatch(request_id)) if request_id == id
    ));
}

#[tokio::test]
async fn get_next_proof_on_empty_queue_returns_none() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Sp1, 0);
    assert!(
        service
            .get_next_proof(get_next_proof_request_for(&req))
            .await
            .unwrap()
            .locked_request
            .is_none()
    );
}

#[tokio::test]
async fn stale_lock_is_rejected_and_reclaim_succeeds() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Sp1, 3);
    let id = service.request_proof(req.clone()).await.unwrap().proof_id;

    let first = service
        .get_next_proof(get_next_proof_request_for(&req))
        .await
        .unwrap()
        .locked_request
        .expect("first lock");

    // Let the first lock expire so the job can be reclaimed.
    expire_proof_lock(&service, id, first.lock_id).await;
    let second = service
        .get_next_proof(get_next_proof_request_for(&req))
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
    let id = service.request_proof(req.clone()).await.unwrap().proof_id;
    let locked = service
        .get_next_proof(get_next_proof_request_for(&req))
        .await
        .unwrap()
        .locked_request
        .expect("lock");

    let mismatched = SucceededProofResponse {
        id,
        proof: ProofData::Nitro {
            attestation: Bytes::from(vec![0xcc]),
            public_values: Bytes::from(vec![0xee]),
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
    let config = test_config();
    let max_attempts = config.max_attempts;
    let expected_failure_reason = format!("proof request exhausted max attempts ({max_attempts})");
    let Some(ctx) = service(config).await else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Sp1, 5);
    let id = service.request_proof(req.clone()).await.unwrap().proof_id;

    let first = service
        .get_next_proof(get_next_proof_request_for(&req))
        .await
        .unwrap()
        .locked_request
        .expect("first lock");
    expire_proof_lock(&service, id, first.lock_id).await;

    let second = service
        .get_next_proof(get_next_proof_request_for(&req))
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

    expire_proof_lock(&service, id, second.lock_id).await;

    // without status poller, this job is not claimable
    assert!(
        service
            .get_next_proof(get_next_proof_request_for(&req))
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
    let id = service.request_proof(req.clone()).await.unwrap().proof_id;
    let locked = service
        .get_next_proof(get_next_proof_request_for(&req))
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

    service
        .record_proof_session(record_proof_session_request(
            id,
            SessionType::Stark,
            locked.lock_id,
            backend_session_id(1),
            BackendSessionStatus::Failed,
            Some("auction timed out".to_string()),
        ))
        .await
        .unwrap();
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
            backend_session_id(2),
            BackendSessionStatus::Running,
            None,
        ))
        .await
        .unwrap();
    let replacement = service
        .get_proof_session(get_proof_session_request(id, SessionType::Stark))
        .await
        .unwrap()
        .session
        .expect("replacement session recorded");
    assert_eq!(replacement.backend_session_id, backend_session_id(2));
    assert_eq!(replacement.status, BackendSessionStatus::Running);

    let rows: Vec<(String, String)> = sqlx::query_as(
        r#"
        SELECT backend_session_id, status
        FROM proof_sessions
        WHERE proof_id = $1 AND session_type = $2
        ORDER BY id
        "#,
    )
    .bind(id.0.as_slice().to_vec())
    .bind(SessionType::Stark.as_str())
    .fetch_all(service.pool())
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (
                backend_session_id(1),
                BackendSessionStatus::Failed.as_str().to_string()
            ),
            (
                backend_session_id(2),
                BackendSessionStatus::Running.as_str().to_string()
            ),
        ]
    );
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
    let id = client.request_proof(req.clone()).await.unwrap().proof_id;
    assert_eq!(client.proof_status(id).await.unwrap(), ProofStatus::Created);

    let locked = client
        .get_next_proof(get_next_proof_request_for(&req))
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
