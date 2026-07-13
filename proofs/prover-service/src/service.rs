use crate::{
    config::ProverServiceConfig,
    error::{InvalidConfigError, ProofJobQueueError, ProofRequestError, ProverServiceInitError},
    store::ProverServiceStore,
    traits::{ProofJobQueue, ProofRequester},
    types::{
        GetNextProofRequest, GetNextProofResponse, GetProofSessionRequest, GetProofSessionResponse,
        HeartbeatRequest, HeartbeatResponse, ProofRequest, ProofRequestId, ProofResponse,
        ProofStatus, RecordProofSessionRequest, RecordProofSessionResponse, SubmitProofRequest,
        SubmitProofResponse,
    },
};
use async_trait::async_trait;
use sqlx::{PgPool, migrate::MigrateError};

/// The central orchestration service between defenders and proof generation backends.
///
/// State is durable in Postgres. All worker-facing mutations validate the lock token returned
/// by the corresponding claim method, so stale workers cannot overwrite rows after their lock
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

    /// Mark proof requests that exhausted all worker attempts as failed.
    pub(crate) async fn mark_exhausted_proof_requests_failed(&self) -> Result<u64, sqlx::Error> {
        self.store.mark_exhausted_proof_requests_failed().await
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
        request: GetNextProofRequest,
    ) -> Result<GetNextProofResponse, ProofJobQueueError> {
        self.store.get_next_proof(request).await
    }

    async fn submit_proof(
        &self,
        request: SubmitProofRequest,
    ) -> Result<SubmitProofResponse, ProofJobQueueError> {
        self.store.submit_proof(request).await
    }

    async fn get_proof_session(
        &self,
        request: GetProofSessionRequest,
    ) -> Result<GetProofSessionResponse, ProofJobQueueError> {
        self.store.get_proof_session(request).await
    }

    async fn record_proof_session(
        &self,
        request: RecordProofSessionRequest,
    ) -> Result<RecordProofSessionResponse, ProofJobQueueError> {
        self.store.record_proof_session(request).await
    }

    async fn heartbeat(
        &self,
        request: HeartbeatRequest,
    ) -> Result<HeartbeatResponse, ProofJobQueueError> {
        self.store.heartbeat(request).await
    }
}
