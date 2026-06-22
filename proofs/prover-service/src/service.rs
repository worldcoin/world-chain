use crate::{
    config::ProverServiceConfig,
    error::{InvalidConfigError, ProofJobQueueError, ProofRequestError, ProverServiceInitError},
    store::ProverServiceStore,
    traits::{ProofJobQueue, ProofRequester},
    types::{
        BackendProofState, BackendUpdate, LeaseToken, LeasedBackendProofWork, LeasedProofRequest,
        ProofBackend, ProofRequest, ProofRequestId, ProofResponse, ProofStatus,
        ProofSubmissionLease,
    },
};
use async_trait::async_trait;
use sqlx::{PgPool, migrate::MigrateError};

/// The central orchestration service between defenders and proof generation backends.
///
/// State is durable in Postgres. All worker-facing mutations validate the lease token returned
/// by the corresponding claim method, so stale workers cannot overwrite rows after their lease
/// expires and another worker takes over.
#[derive(Debug, Clone)]
pub struct ProverService {
    store: ProverServiceStore,
}

impl ProverService {
    /// Create a service over an existing Postgres pool.
    pub fn new(pool: PgPool, config: ProverServiceConfig) -> Result<Self, InvalidConfigError> {
        Ok(Self {
            store: ProverServiceStore::new(pool, config)?,
        })
    }

    /// Connect to Postgres, run migrations, and create the service.
    pub async fn connect(
        database_url: &str,
        config: ProverServiceConfig,
    ) -> Result<Self, ProverServiceInitError> {
        Ok(Self {
            store: ProverServiceStore::connect(database_url, config).await?,
        })
    }

    /// Run database migrations for the prover-service schema.
    pub async fn migrate(pool: &PgPool) -> Result<(), MigrateError> {
        ProverServiceStore::migrate(pool).await
    }

    /// Access the underlying pool.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        self.store.pool()
    }
}

#[async_trait]
impl ProofRequester for ProverService {
    async fn request_proof(
        &self,
        proof_request: ProofRequest,
    ) -> Result<ProofRequestId, ProofRequestError> {
        self.store.request_proof(proof_request).await
    }

    async fn proof_status(
        &self,
        proof_id: ProofRequestId,
    ) -> Result<ProofStatus, ProofRequestError> {
        self.store.proof_status(proof_id).await
    }

    async fn get_proof(
        &self,
        proof_id: ProofRequestId,
    ) -> Result<ProofResponse, ProofRequestError> {
        self.store.get_proof(proof_id).await
    }
}

#[async_trait]
impl ProofJobQueue for ProverService {
    async fn get_next_proof(
        &self,
        backend: ProofBackend,
    ) -> Result<Option<LeasedProofRequest>, ProofJobQueueError> {
        self.store.get_next_proof(backend).await
    }

    async fn submit_backend_proof_state(
        &self,
        proof_id: ProofRequestId,
        backend_proof_state: BackendProofState,
        lease_token: LeaseToken,
    ) -> Result<(), ProofJobQueueError> {
        self.store
            .submit_backend_proof_state(proof_id, backend_proof_state, lease_token)
            .await
    }

    async fn get_next_backend_proof(
        &self,
        backend: ProofBackend,
    ) -> Result<Option<LeasedBackendProofWork>, ProofJobQueueError> {
        self.store.get_next_backend_proof(backend).await
    }

    async fn complete_backend_proof_job(
        &self,
        backend_job_id: i64,
        lease_token: LeaseToken,
        next_update: BackendUpdate,
    ) -> Result<(), ProofJobQueueError> {
        match next_update {
            BackendUpdate::Noop => {
                self.store
                    .noop_backend_job(backend_job_id, lease_token)
                    .await
            }
            BackendUpdate::Pending { state } => {
                self.store
                    .advance_backend_job(backend_job_id, lease_token, state)
                    .await
            }
            BackendUpdate::Failed(reason) => {
                self.store
                    .fail_backend_job(backend_job_id, lease_token, &reason)
                    .await
            }
            BackendUpdate::Complete(proof) => {
                self.store
                    .submit_completed_backend_proof(backend_job_id, lease_token, proof)
                    .await
            }
        }
    }

    async fn fail_backend_proof_job(
        &self,
        backend_job_id: i64,
        reason: String,
        lease_token: LeaseToken,
    ) -> Result<(), ProofJobQueueError> {
        self.store
            .fail_backend_proof_job(backend_job_id, reason, lease_token)
            .await
    }

    async fn submit_proof(
        &self,
        proof: ProofResponse,
        lease: ProofSubmissionLease,
    ) -> Result<(), ProofJobQueueError> {
        match lease {
            ProofSubmissionLease::ProofJob { lease_token } => {
                self.store
                    .submit_proof_from_proof_job(proof, lease_token)
                    .await
            }
            ProofSubmissionLease::BackendJob {
                backend_job_id,
                lease_token,
            } => {
                self.store
                    .submit_proof_from_backend_job(proof, backend_job_id, lease_token)
                    .await
            }
        }
    }

    async fn fail_proof(
        &self,
        proof_id: ProofRequestId,
        reason: String,
        lease_token: LeaseToken,
    ) -> Result<(), ProofJobQueueError> {
        self.store.fail_proof(proof_id, reason, lease_token).await
    }
}
