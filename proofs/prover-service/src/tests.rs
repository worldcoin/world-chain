use crate::{
    BackendProofId, BackendProofState, BackendUpdate, LeasedProofRequest, ProofBackend, ProofData,
    ProofJobQueue, ProofJobQueueError, ProofRequest, ProofRequestError, ProofRequester,
    ProofResponse, ProofStatus, ProofSubmissionLease, ProverService, ProverServiceConfig,
    RpcProverServiceClient, start_rpc_server,
};
use alloy_primitives::{Address, B256, Bytes};
use std::{sync::Arc, time::Duration};
use testcontainers::{ContainerAsync, runners::AsyncRunner};
use testcontainers_modules::postgres;

fn test_config() -> ProverServiceConfig {
    ProverServiceConfig {
        lease_timeout: Duration::from_secs(60),
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

    let leased = service
        .get_next_proof(ProofBackend::Nitro)
        .await
        .unwrap()
        .expect("job available");
    assert_eq!(leased.request, req);
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
            ProofSubmissionLease::ProofJob {
                lease_token: leased.lease_token,
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
    let LeasedProofRequest {
        request: leased_req,
        lease_token,
    } = service
        .get_next_proof(ProofBackend::Sp1)
        .await
        .unwrap()
        .expect("job available");
    assert_eq!(leased_req, req);

    service
        .submit_backend_proof_state(
            id,
            BackendProofState::Range { id: backend_id(1) },
            lease_token,
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
        .complete_backend_proof_job(range.backend_job_id, range.lease_token, BackendUpdate::Noop)
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
            range.lease_token,
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
            ProofSubmissionLease::BackendJob {
                backend_job_id: aggregation.backend_job_id,
                lease_token: aggregation.lease_token,
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
async fn stale_start_lease_is_rejected() {
    let Some(ctx) = service(ProverServiceConfig {
        lease_timeout: Duration::from_millis(10),
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
        .get_next_proof(ProofBackend::Sp1)
        .await
        .unwrap()
        .expect("first lease");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let second = service
        .get_next_proof(ProofBackend::Sp1)
        .await
        .unwrap()
        .expect("second lease");

    assert!(matches!(
        service
            .submit_proof(
                proof_for(&req),
                ProofSubmissionLease::ProofJob {
                    lease_token: first.lease_token,
                },
            )
            .await,
        Err(ProofJobQueueError::StaleLease)
    ));

    service
        .submit_proof(
            proof_for(&req),
            ProofSubmissionLease::ProofJob {
                lease_token: second.lease_token,
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
        .get_next_proof(ProofBackend::Sp1)
        .await
        .unwrap()
        .expect("first lease");
    service
        .fail_proof(
            id,
            "witness generation failed".to_string(),
            first.lease_token,
        )
        .await
        .unwrap();
    assert_eq!(service.proof_status(id).await.unwrap(), ProofStatus::Queued);

    let second = service
        .get_next_proof(ProofBackend::Sp1)
        .await
        .unwrap()
        .expect("second lease");
    service
        .fail_proof(id, "backend rejected".to_string(), second.lease_token)
        .await
        .unwrap();
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

    let leased = client
        .get_next_proof(ProofBackend::Sp1)
        .await
        .unwrap()
        .expect("job available");
    assert_eq!(leased.request, req);
    client
        .submit_proof(
            proof_for(&req),
            ProofSubmissionLease::ProofJob {
                lease_token: leased.lease_token,
            },
        )
        .await
        .unwrap();
    assert_eq!(client.get_proof(id).await.unwrap(), proof_for(&req));

    handle.stop().unwrap();
    handle.stopped().await;
}
