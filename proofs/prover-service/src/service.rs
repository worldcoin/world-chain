use crate::{
    config::ProverServiceConfig,
    error::{InvalidConfigError, ProofJobQueueError, ProofRequestError, ProverServiceInitError},
    store::ProverServiceStore,
    traits::{ProofJobQueue, ProofRequester},
    types::{
        BackendSession, BackendSessionStatus, LockId, LockedProofRequest, ProofBackend,
        ProofRequest, ProofRequestId, ProofResponse, ProofStatus, SessionType,
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
        worker_id: String,
    ) -> Result<Option<LockedProofRequest>, ProofJobQueueError> {
        self.store.get_next_proof(backend, worker_id).await
    }

    async fn submit_proof(
        &self,
        proof: ProofResponse,
        worker_id: String,
        lock: LockId,
    ) -> Result<(), ProofJobQueueError> {
        self.store.submit_proof(proof, worker_id, lock).await
    }

    async fn get_proof_session(
        &self,
        proof_id: ProofRequestId,
        session_type: SessionType,
    ) -> Result<Option<BackendSession>, ProofJobQueueError> {
        self.store.get_proof_session(proof_id, session_type).await
    }

    async fn record_proof_session(
        &self,
        proof_id: ProofRequestId,
        session_type: SessionType,
        worker_id: String,
        lock_id: LockId,
        backend_session_id: String,
        state: BackendSessionStatus,
    ) -> Result<(), ProofJobQueueError> {
        self.record_proof_session(
            proof_id,
            session_type,
            worker_id,
            lock_id,
            backend_session_id,
            state,
        )
        .await
    }
}
