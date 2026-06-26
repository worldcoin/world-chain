use crate::{
    BackendProofId, BackendProofState, BackendUpdate, LockedProofRequest, ProofBackend, ProofData,
    ProofJobQueue, ProofJobQueueError, ProofRequest, ProofRequestError, ProofRequester,
    ProofResponse, ProofStatus, ProofSubmissionLock, ProverService, ProverServiceConfig,
    RpcProverServiceClient, start_rpc_server,
};
use alloy_primitives::{Address, B256, Bytes};
use sqlx::Row;
use std::{sync::Arc, time::Duration};
use testcontainers::{ContainerAsync, runners::AsyncRunner};
use testcontainers_modules::postgres;

fn test_config() -> ProverServiceConfig {
    ProverServiceConfig {
        lock_timeout: Duration::from_secs(60),
        max_attempts: 2,
        max_queue_len: 4,
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

fn backend_id(seed: u8) -> BackendProofId {
    BackendProofId(B256::with_last_byte(seed))
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
async fn nitro_style_full_lifecycle() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Nitro, 1);

    let id = service.request_proof(req.clone()).await.unwrap();
    assert_eq!(service.proof_status(id).await.unwrap(), ProofStatus::Queued);

    let locked = service
        .get_next_proof(ProofBackend::Nitro, worker_id())
        .await
        .unwrap()
        .expect("job available");
    assert_eq!(locked.request, req);
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::Starting
    );
    assert!(matches!(
        service.get_proof(id).await,
        Err(ProofRequestError::Pending {
            status: ProofStatus::Starting,
            ..
        })
    ));

    let response = proof_for(&req);
    service
        .submit_proof(
            response.clone(),
            ProofSubmissionLock::ProofJob {
                lock_id: locked.lock_id,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::Completed
    );
    assert_eq!(service.get_proof(id).await.unwrap(), response);
}

#[tokio::test]
async fn backend_job_workflow_reaches_final_proof() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Sp1, 2);
    let id = service.request_proof(req.clone()).await.unwrap();
    let LockedProofRequest {
        request: locked_req,
        lock_id,
    } = service
        .get_next_proof(ProofBackend::Sp1, worker_id())
        .await
        .unwrap()
        .expect("job available");
    assert_eq!(locked_req, req);

    service
        .submit_backend_proof_state(
            id,
            BackendProofState::Range { id: backend_id(1) },
            lock_id,
            worker_id(),
        )
        .await
        .unwrap();
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::BackendPending
    );

    let range = service
        .get_next_backend_proof(ProofBackend::Sp1)
        .await
        .unwrap()
        .expect("range job");
    assert_eq!(range.work.proof_request, req);
    assert_eq!(
        range.work.state,
        BackendProofState::Range { id: backend_id(1) }
    );

    service
        .complete_backend_proof_job(range.backend_job_id, range.lock_id, BackendUpdate::Noop)
        .await
        .unwrap();
    assert!(
        service
            .get_next_backend_proof(ProofBackend::Sp1)
            .await
            .unwrap()
            .is_none()
    );
    tokio::time::sleep(Duration::from_millis(30)).await;

    let range = service
        .get_next_backend_proof(ProofBackend::Sp1)
        .await
        .unwrap()
        .expect("range job after poll delay");
    service
        .complete_backend_proof_job(
            range.backend_job_id,
            range.lock_id,
            BackendUpdate::Pending {
                state: BackendProofState::Aggregation { id: backend_id(2) },
            },
        )
        .await
        .unwrap();

    let aggregation = service
        .get_next_backend_proof(ProofBackend::Sp1)
        .await
        .unwrap()
        .expect("aggregation job");
    assert_eq!(
        aggregation.work.state,
        BackendProofState::Aggregation { id: backend_id(2) }
    );
    service
        .submit_proof(
            proof_for(&req),
            ProofSubmissionLock::BackendJob {
                backend_job_id: aggregation.backend_job_id,
                lock_id: aggregation.lock_id,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::Completed
    );
}

#[tokio::test]
async fn stale_start_lock_is_rejected() {
    let Some(ctx) = service(ProverServiceConfig {
        lock_timeout: Duration::from_millis(10),
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
    tokio::time::sleep(Duration::from_millis(20)).await;
    let second = service
        .get_next_proof(ProofBackend::Sp1, worker_id())
        .await
        .unwrap()
        .expect("second lock");

    assert!(matches!(
        service
            .submit_proof(
                proof_for(&req),
                ProofSubmissionLock::ProofJob {
                    lock_id: first.lock_id,
                },
            )
            .await,
        Err(ProofJobQueueError::StaleLocked)
    ));

    service
        .submit_proof(
            proof_for(&req),
            ProofSubmissionLock::ProofJob {
                lock_id: second.lock_id,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::Completed
    );
}

#[tokio::test]
async fn failed_attempts_retry_until_exhausted() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Sp1, 4);
    let id = service.request_proof(req.clone()).await.unwrap();

    let first = service
        .get_next_proof(ProofBackend::Sp1, worker_id())
        .await
        .unwrap()
        .expect("first lock");
    service
        .fail_proof(
            id,
            "witness generation failed".to_string(),
            first.lock_id,
            worker_id(),
        )
        .await
        .unwrap();
    assert_eq!(service.proof_status(id).await.unwrap(), ProofStatus::Queued);

    let second = service
        .get_next_proof(ProofBackend::Sp1, worker_id())
        .await
        .unwrap()
        .expect("second lock");
    service
        .fail_proof(
            id,
            "backend rejected".to_string(),
            second.lock_id,
            worker_id(),
        )
        .await
        .unwrap();
    assert_eq!(service.proof_status(id).await.unwrap(), ProofStatus::Failed);
}

#[tokio::test]
async fn backend_attempt_errors_retry_until_exhausted() {
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
        .expect("start lock");
    service
        .submit_backend_proof_state(
            id,
            BackendProofState::Range { id: backend_id(1) },
            locked.lock_id,
            worker_id(),
        )
        .await
        .unwrap();

    let first = service
        .get_next_backend_proof(ProofBackend::Sp1)
        .await
        .unwrap()
        .expect("first backend lock");
    service
        .fail_backend_proof_job(
            first.backend_job_id,
            "temporary SP1 poll failure".to_string(),
            first.lock_id,
        )
        .await
        .unwrap();
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::BackendPending
    );
    assert!(
        service
            .get_next_backend_proof(ProofBackend::Sp1)
            .await
            .unwrap()
            .is_none()
    );
    tokio::time::sleep(Duration::from_millis(30)).await;

    let second = service
        .get_next_backend_proof(ProofBackend::Sp1)
        .await
        .unwrap()
        .expect("second backend lock");
    assert_eq!(second.backend_job_id, first.backend_job_id);
    service
        .fail_backend_proof_job(
            second.backend_job_id,
            "temporary SP1 poll failure".to_string(),
            second.lock_id,
        )
        .await
        .unwrap();
    assert_eq!(service.proof_status(id).await.unwrap(), ProofStatus::Failed);
}

#[tokio::test]
async fn invalid_completed_backend_proof_fails_immediately() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Sp1, 6);
    let id = service.request_proof(req.clone()).await.unwrap();
    let locked = service
        .get_next_proof(ProofBackend::Sp1, worker_id())
        .await
        .unwrap()
        .expect("start lock");
    service
        .submit_backend_proof_state(
            id,
            BackendProofState::Range { id: backend_id(1) },
            locked.lock_id,
            worker_id(),
        )
        .await
        .unwrap();

    let backend = service
        .get_next_backend_proof(ProofBackend::Sp1)
        .await
        .unwrap()
        .expect("backend lock");
    let invalid_proof = ProofData::Nitro {
        attestation: Bytes::from(vec![0xcc]),
        signature: Bytes::from(vec![0xdd]),
    };
    assert!(matches!(
        service
            .complete_backend_proof_job(
                backend.backend_job_id,
                backend.lock_id,
                BackendUpdate::Complete(invalid_proof),
            )
            .await,
        Err(ProofJobQueueError::InvalidProof { .. })
    ));

    assert_eq!(service.proof_status(id).await.unwrap(), ProofStatus::Failed);
    assert!(
        service
            .get_next_backend_proof(ProofBackend::Sp1)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn failed_completed_backend_submission_relocks_lock() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Sp1, 7);
    let id = service.request_proof(req.clone()).await.unwrap();
    let locked = service
        .get_next_proof(ProofBackend::Sp1, worker_id())
        .await
        .unwrap()
        .expect("start lock");
    service
        .submit_backend_proof_state(
            id,
            BackendProofState::Range { id: backend_id(1) },
            locked.lock_id,
            worker_id(),
        )
        .await
        .unwrap();
    let backend = service
        .get_next_backend_proof(ProofBackend::Sp1)
        .await
        .unwrap()
        .expect("backend lock");

    sqlx::query("update proof_requests set backend = 'corrupt' where proof_id = $1")
        .bind(id.0.as_slice())
        .execute(service.pool())
        .await
        .unwrap();

    assert!(matches!(
        service
            .complete_backend_proof_job(
                backend.backend_job_id,
                backend.lock_id,
                BackendUpdate::Complete(proof_for(&req).proof),
            )
            .await,
        Err(ProofJobQueueError::Internal(_))
    ));

    let row = sqlx::query(
        "select status, advance_attempts, lock_expires_at is null as unlocked,
                lock_id is null as lock_cleared
         from proof_sessions
         where id = $1",
    )
    .bind(backend.backend_job_id)
    .fetch_one(service.pool())
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("status"), "requested");
    assert_eq!(row.get::<i32, _>("advance_attempts"), 1);
    assert!(row.get::<bool, _>("unlocked"));
    assert!(row.get::<bool, _>("lock_cleared"));
}

#[tokio::test]
async fn backend_noop_polling_does_not_exhaust_attempts() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Sp1, 8);
    let id = service.request_proof(req.clone()).await.unwrap();
    let locked = service
        .get_next_proof(ProofBackend::Sp1, worker_id())
        .await
        .unwrap()
        .expect("start lock");
    service
        .submit_backend_proof_state(
            id,
            BackendProofState::Range { id: backend_id(1) },
            locked.lock_id,
            worker_id(),
        )
        .await
        .unwrap();

    let mut backend_job_id = None;
    for _ in 0..4 {
        let backend = service
            .get_next_backend_proof(ProofBackend::Sp1)
            .await
            .unwrap()
            .expect("backend lock");
        if let Some(previous) = backend_job_id {
            assert_eq!(backend.backend_job_id, previous);
        } else {
            backend_job_id = Some(backend.backend_job_id);
        }
        service
            .complete_backend_proof_job(
                backend.backend_job_id,
                backend.lock_id,
                BackendUpdate::Noop,
            )
            .await
            .unwrap();
        assert_eq!(
            service.proof_status(id).await.unwrap(),
            ProofStatus::BackendPending
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    let backend = service
        .get_next_backend_proof(ProofBackend::Sp1)
        .await
        .unwrap()
        .expect("backend lock after repeated noops");
    service
        .complete_backend_proof_job(
            backend.backend_job_id,
            backend.lock_id,
            BackendUpdate::Pending {
                state: BackendProofState::Aggregation { id: backend_id(2) },
            },
        )
        .await
        .unwrap();
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::BackendPending
    );
}

#[tokio::test]
async fn expired_backend_locks_count_toward_attempts() {
    let Some(ctx) = service(ProverServiceConfig {
        lock_timeout: Duration::from_millis(10),
        ..test_config()
    })
    .await
    else {
        return;
    };
    let service = ctx.service;
    let req = request(ProofBackend::Sp1, 9);
    let id = service.request_proof(req.clone()).await.unwrap();
    let locked = service
        .get_next_proof(ProofBackend::Sp1, worker_id())
        .await
        .unwrap()
        .expect("start lock");
    service
        .submit_backend_proof_state(
            id,
            BackendProofState::Range { id: backend_id(1) },
            locked.lock_id,
            worker_id(),
        )
        .await
        .unwrap();

    let first = service
        .get_next_backend_proof(ProofBackend::Sp1)
        .await
        .unwrap()
        .expect("first backend lock");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let second = service
        .get_next_backend_proof(ProofBackend::Sp1)
        .await
        .unwrap()
        .expect("second backend lock");
    assert_eq!(second.backend_job_id, first.backend_job_id);
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::BackendPending
    );

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        service
            .get_next_backend_proof(ProofBackend::Sp1)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(service.proof_status(id).await.unwrap(), ProofStatus::Failed);
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

    let req = request(ProofBackend::Sp1, 5);
    let id = client.request_proof(req.clone()).await.unwrap();
    assert_eq!(client.proof_status(id).await.unwrap(), ProofStatus::Queued);

    let locked = client
        .get_next_proof(ProofBackend::Sp1, worker_id())
        .await
        .unwrap()
        .expect("job available");
    assert_eq!(locked.request, req);
    client
        .submit_proof(
            proof_for(&req),
            ProofSubmissionLock::ProofJob {
                lock_id: locked.lock_id,
            },
        )
        .await
        .unwrap();
    assert_eq!(client.get_proof(id).await.unwrap(), proof_for(&req));

    handle.stop().unwrap();
    handle.stopped().await;
}

#[tokio::test]
async fn rpc_backend_invalid_proof_maps_to_typed_error() {
    let Some(ctx) = service(test_config()).await else {
        return;
    };
    let service = Arc::new(ctx.service);
    let (addr, handle) = start_rpc_server("127.0.0.1:0".parse().unwrap(), Arc::clone(&service))
        .await
        .unwrap();
    let client = RpcProverServiceClient::new(format!("http://{addr}")).unwrap();

    let req = request(ProofBackend::Sp1, 10);
    let id = client.request_proof(req.clone()).await.unwrap();
    let locked = client
        .get_next_proof(ProofBackend::Sp1, worker_id())
        .await
        .unwrap()
        .expect("start lock");
    client
        .submit_backend_proof_state(
            id,
            BackendProofState::Range { id: backend_id(1) },
            locked.lock_id,
            worker_id(),
        )
        .await
        .unwrap();
    let backend = client
        .get_next_backend_proof(ProofBackend::Sp1)
        .await
        .unwrap()
        .expect("backend lock");

    let invalid_proof = ProofData::Nitro {
        attestation: Bytes::from(vec![0xcc]),
        signature: Bytes::from(vec![0xdd]),
    };
    assert!(matches!(
        client
            .complete_backend_proof_job(
                backend.backend_job_id,
                backend.lock_id,
                BackendUpdate::Complete(invalid_proof),
            )
            .await,
        Err(ProofJobQueueError::InvalidProof { id: error_id, .. }) if error_id == id
    ));

    handle.stop().unwrap();
    handle.stopped().await;
}
