use crate::{
    InvalidConfigError, ProofBackend, ProofData, ProofJobQueue, ProofJobQueueError, ProofRequest,
    ProofRequestError, ProofRequester, ProofResponse, ProofStatus, ProverService,
    ProverServiceConfig, RpcProverServiceClient, start_rpc_server,
};
use alloy_primitives::{Address, B256, Bytes};
use std::{sync::Arc, time::Duration};

fn test_config() -> ProverServiceConfig {
    ProverServiceConfig {
        lease_timeout: Duration::from_secs(60),
        max_attempts: 2,
        max_queue_len: 4,
        max_finished_jobs: 4,
    }
}

fn service(config: ProverServiceConfig) -> ProverService {
    ProverService::new(config).expect("valid config")
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

#[test]
fn invalid_config_is_rejected() {
    let config = ProverServiceConfig {
        max_attempts: 0,
        ..test_config()
    };
    assert!(matches!(
        ProverService::new(config),
        Err(InvalidConfigError(_))
    ));
}

#[test]
fn request_id_is_deterministic() {
    let sp1 = request(ProofBackend::Sp1, 1);
    assert_eq!(sp1.id(), request(ProofBackend::Sp1, 1).id());
    assert_ne!(sp1.id(), request(ProofBackend::Nitro, 1).id());
    assert_ne!(sp1.id(), request(ProofBackend::Sp1, 2).id());
}

#[tokio::test]
async fn full_lifecycle() {
    let service = service(test_config());
    let req = request(ProofBackend::Sp1, 1);

    let id = service.request_proof(req.clone()).await.unwrap();
    assert_eq!(id, req.id());
    assert_eq!(service.proof_status(id).await.unwrap(), ProofStatus::Queued);

    let leased = service
        .get_next_proof(ProofBackend::Sp1)
        .await
        .unwrap()
        .expect("job available");
    assert_eq!(leased, req);
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::InProgress
    );
    assert!(matches!(
        service.get_proof(id).await,
        Err(ProofRequestError::Pending {
            status: ProofStatus::InProgress,
            ..
        })
    ));

    let response = proof_for(&req);
    service.submit_proof(response.clone()).await.unwrap();
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::Completed
    );
    assert_eq!(service.get_proof(id).await.unwrap(), response);

    // A duplicate submission is a no-op.
    service.submit_proof(response).await.unwrap();
}

#[tokio::test]
async fn jobs_are_routed_per_backend() {
    let service = service(test_config());
    let sp1 = request(ProofBackend::Sp1, 1);
    let nitro = request(ProofBackend::Nitro, 2);
    service.request_proof(sp1.clone()).await.unwrap();
    service.request_proof(nitro.clone()).await.unwrap();

    assert_eq!(
        service.get_next_proof(ProofBackend::Nitro).await.unwrap(),
        Some(nitro)
    );
    assert_eq!(
        service.get_next_proof(ProofBackend::Nitro).await.unwrap(),
        None
    );
    assert_eq!(
        service.get_next_proof(ProofBackend::Sp1).await.unwrap(),
        Some(sp1)
    );
}

#[tokio::test]
async fn duplicate_requests_are_deduplicated() {
    let service = service(test_config());
    let req = request(ProofBackend::Sp1, 1);

    let id = service.request_proof(req.clone()).await.unwrap();
    assert_eq!(service.request_proof(req.clone()).await.unwrap(), id);

    // Only one job was queued.
    assert!(
        service
            .get_next_proof(ProofBackend::Sp1)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        service.get_next_proof(ProofBackend::Sp1).await.unwrap(),
        None
    );

    // Re-requesting while in progress is also a no-op.
    assert_eq!(service.request_proof(req).await.unwrap(), id);
    assert_eq!(
        service.get_next_proof(ProofBackend::Sp1).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn expired_lease_is_requeued_then_failed() {
    let service = service(ProverServiceConfig {
        lease_timeout: Duration::from_millis(10),
        max_attempts: 2,
        ..test_config()
    });
    let req = request(ProofBackend::Sp1, 1);
    let id = service.request_proof(req.clone()).await.unwrap();

    // First lease expires and the job goes back to the queue.
    assert!(
        service
            .get_next_proof(ProofBackend::Sp1)
            .await
            .unwrap()
            .is_some()
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        service.get_next_proof(ProofBackend::Sp1).await.unwrap(),
        Some(req)
    );

    // Second lease expires too, exhausting `max_attempts`.
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        service.get_next_proof(ProofBackend::Sp1).await.unwrap(),
        None
    );
    assert_eq!(service.proof_status(id).await.unwrap(), ProofStatus::Failed);
    assert!(matches!(
        service.get_proof(id).await,
        Err(ProofRequestError::Failed { .. })
    ));
}

#[tokio::test]
async fn failed_attempts_are_retried_until_exhausted() {
    let service = service(test_config());
    let req = request(ProofBackend::Sp1, 1);
    let id = service.request_proof(req.clone()).await.unwrap();

    // First attempt fails and the job is re-queued.
    service.get_next_proof(ProofBackend::Sp1).await.unwrap();
    service
        .fail_proof(id, "witness generation failed".to_string())
        .await
        .unwrap();
    assert_eq!(service.proof_status(id).await.unwrap(), ProofStatus::Queued);

    // Second attempt fails and exhausts `max_attempts`.
    service.get_next_proof(ProofBackend::Sp1).await.unwrap();
    service
        .fail_proof(id, "enclave rejected".to_string())
        .await
        .unwrap();
    assert_eq!(service.proof_status(id).await.unwrap(), ProofStatus::Failed);
    match service.get_proof(id).await {
        Err(ProofRequestError::Failed { reason, .. }) => {
            assert_eq!(reason, "enclave rejected");
        }
        other => panic!("expected failed proof, got {other:?}"),
    }

    // Re-requesting a failed proof re-queues it from scratch.
    assert_eq!(service.request_proof(req.clone()).await.unwrap(), id);
    assert_eq!(service.proof_status(id).await.unwrap(), ProofStatus::Queued);
    assert_eq!(
        service.get_next_proof(ProofBackend::Sp1).await.unwrap(),
        Some(req)
    );
}

#[tokio::test]
async fn late_submission_after_requeue_is_accepted() {
    let service = service(ProverServiceConfig {
        lease_timeout: Duration::from_millis(10),
        ..test_config()
    });
    let req = request(ProofBackend::Sp1, 1);
    let id = service.request_proof(req.clone()).await.unwrap();

    service.get_next_proof(ProofBackend::Sp1).await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    // Lease expired and the job is back in the queue, but the original
    // worker still delivers a valid proof.
    service.submit_proof(proof_for(&req)).await.unwrap();
    assert_eq!(
        service.proof_status(id).await.unwrap(),
        ProofStatus::Completed
    );

    // The stale queue entry is skipped.
    assert_eq!(
        service.get_next_proof(ProofBackend::Sp1).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn backend_mismatch_is_rejected() {
    let service = service(test_config());
    let req = request(ProofBackend::Sp1, 1);
    let id = service.request_proof(req.clone()).await.unwrap();
    service.get_next_proof(ProofBackend::Sp1).await.unwrap();

    let wrong = ProofResponse {
        id,
        proof: ProofData::Nitro {
            attestation: Bytes::new(),
            signature: Bytes::new(),
        },
    };
    assert!(matches!(
        service.submit_proof(wrong).await,
        Err(ProofJobQueueError::InvalidProof { .. })
    ));
}

#[tokio::test]
async fn unknown_job_is_rejected() {
    let service = service(test_config());
    let req = request(ProofBackend::Sp1, 1);
    assert!(matches!(
        service.submit_proof(proof_for(&req)).await,
        Err(ProofJobQueueError::UnknownJob(_))
    ));
    assert!(matches!(
        service.fail_proof(req.id(), "nope".to_string()).await,
        Err(ProofJobQueueError::UnknownJob(_))
    ));
    assert!(matches!(
        service.get_proof(req.id()).await,
        Err(ProofRequestError::NotFound(_))
    ));
}

#[tokio::test]
async fn full_queue_rejects_new_requests() {
    let service = service(ProverServiceConfig {
        max_queue_len: 1,
        ..test_config()
    });
    service
        .request_proof(request(ProofBackend::Sp1, 1))
        .await
        .unwrap();
    assert!(matches!(
        service.request_proof(request(ProofBackend::Sp1, 2)).await,
        Err(ProofRequestError::QueueFull(ProofBackend::Sp1))
    ));
    // Other backends are unaffected.
    service
        .request_proof(request(ProofBackend::Nitro, 3))
        .await
        .unwrap();
}

#[tokio::test]
async fn oldest_finished_jobs_are_evicted() {
    let service = service(ProverServiceConfig {
        max_finished_jobs: 1,
        ..test_config()
    });
    let first = request(ProofBackend::Sp1, 1);
    let second = request(ProofBackend::Sp1, 2);
    for req in [&first, &second] {
        let id = service.request_proof(req.clone()).await.unwrap();
        service.get_next_proof(ProofBackend::Sp1).await.unwrap();
        service.submit_proof(proof_for(req)).await.unwrap();
        assert_eq!(
            service.proof_status(id).await.unwrap(),
            ProofStatus::Completed
        );
    }

    // Completing the second proof evicted the first.
    assert!(matches!(
        service.get_proof(first.id()).await,
        Err(ProofRequestError::NotFound(_))
    ));
    assert!(service.get_proof(second.id()).await.is_ok());
}

#[tokio::test]
async fn rpc_end_to_end() {
    let service = Arc::new(service(test_config()));
    let (addr, handle) = start_rpc_server("127.0.0.1:0".parse().unwrap(), service)
        .await
        .unwrap();
    let client = RpcProverServiceClient::new(format!("http://{addr}")).unwrap();

    let req = request(ProofBackend::Sp1, 1);
    let missing = request(ProofBackend::Sp1, 9).id();

    // Defender-facing surface, including error mapping.
    let id = client.request_proof(req.clone()).await.unwrap();
    assert_eq!(id, req.id());
    assert_eq!(client.proof_status(id).await.unwrap(), ProofStatus::Queued);
    assert!(matches!(
        client.get_proof(missing).await,
        Err(ProofRequestError::NotFound(found)) if found == missing
    ));
    assert!(matches!(
        client.get_proof(id).await,
        Err(ProofRequestError::Pending {
            status: ProofStatus::Queued,
            ..
        })
    ));

    // Worker-facing surface.
    assert_eq!(
        client.get_next_proof(ProofBackend::Nitro).await.unwrap(),
        None
    );
    let leased = client
        .get_next_proof(ProofBackend::Sp1)
        .await
        .unwrap()
        .expect("job available");
    assert_eq!(leased, req);
    assert!(matches!(
        client.submit_proof(proof_for(&request(ProofBackend::Sp1, 9))).await,
        Err(ProofJobQueueError::UnknownJob(unknown)) if unknown == missing
    ));
    client.submit_proof(proof_for(&req)).await.unwrap();

    let response = client.get_proof(id).await.unwrap();
    assert_eq!(response, proof_for(&req));

    handle.stop().unwrap();
    handle.stopped().await;
}
