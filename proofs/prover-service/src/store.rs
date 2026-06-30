use crate::{
    config::ProverServiceConfig,
    error::{InvalidConfigError, ProofJobQueueError, ProofRequestError, ProverServiceInitError},
    types::{
        BackendSession, BackendSessionStatus, LockId, LockedProofRequest, ProofBackend,
        ProofJobStatus, ProofRequest, ProofRequestId, ProofResponse, ProofStatus, SessionType,
    },
};
use alloy_primitives::{Address, B256};
use chrono::Utc;
use sqlx::{
    PgPool, Postgres, Row, Transaction,
    migrate::{MigrateError, Migrator},
    postgres::{PgPoolOptions, PgRow},
};
use uuid::Uuid;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, Clone)]
pub(crate) struct ProverServiceStore {
    config: ProverServiceConfig,
    pool: PgPool,
}

#[derive(Debug, Clone, Copy)]
enum PostgresIsolationLevel {
    ReadCommitted,
}

impl PostgresIsolationLevel {
    const fn set_transaction_sql(self) -> &'static str {
        match self {
            Self::ReadCommitted => "SET TRANSACTION ISOLATION LEVEL READ COMMITTED",
        }
    }
}

impl ProverServiceStore {
    pub(crate) fn new(
        pool: PgPool,
        config: ProverServiceConfig,
    ) -> Result<Self, InvalidConfigError> {
        config.validate()?;
        Ok(Self { config, pool })
    }

    pub(crate) async fn connect(
        database_url: &str,
        config: ProverServiceConfig,
    ) -> Result<Self, ProverServiceInitError> {
        config.validate()?;
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await?;
        Self::migrate(&pool).await?;
        Ok(Self { config, pool })
    }

    pub(crate) async fn migrate(pool: &PgPool) -> Result<(), MigrateError> {
        MIGRATOR.run(pool).await
    }

    #[must_use]
    pub(crate) const fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub(crate) async fn request_proof(
        &self,
        proof_request: ProofRequest,
    ) -> Result<ProofRequestId, ProofRequestError> {
        let id = proof_request.id();
        let backend = proof_request.backend;
        let proof_id = proof_id_bytes(id);
        let now = Utc::now();
        let mut tx = self.begin_request_tx().await?;

        // --
        let insert_result = sqlx::query(
            r#"
            INSERT INTO proof_requests (
                proof_id, backend, game, root_claim, l2_block_number, l1_head,
                proof_status, job_status, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            ON CONFLICT (proof_id) DO NOTHING
            "#,
        )
        .bind(&proof_id)
        .bind(backend.as_str())
        .bind(proof_request.game.as_slice())
        .bind(proof_request.root_claim.as_slice())
        .bind(l2_to_i64(proof_request.l2_block_number)?)
        .bind(proof_request.l1_head.as_slice())
        .bind(ProofStatus::Created.as_str())
        .bind(ProofJobStatus::Pending.as_str())
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(request_db)?;

        if insert_result.rows_affected() > 0 {
            // no conflict
            tx.commit().await.map_err(request_db)?;
            return Ok(id);
        }

        // conflict path
        let row = sqlx::query(
            r#"
            SELECT proof_status, retry_count
            FROM proof_requests
            WHERE proof_id = $1
            FOR UPDATE
            "#,
        )
        .bind(&proof_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(request_db)?;

        let Some(row) = row else {
            tx.rollback().await.map_err(request_db)?;
            return Err(ProofRequestError::RowMissingAfterConflict { id });
        };

        let proof_status_str: &str = row.get("proof_status");
        let proof_status = ProofStatus::try_from(proof_status_str).map_err(|e| {
            ProofRequestError::Internal(format!("Invalid proof status in db: {}", e))
        })?;

        if proof_status == ProofStatus::Failed {
            // retry the entire proof job if retry_count is less than the max_retry
            let retry_count: i32 = row.get("retry_count");
            // TODO: eventually this max can be a cli parameter
            if retry_count > self.config.max_retries as i32 {
                tx.rollback().await.map_err(request_db)?;
                return Err(ProofRequestError::TooManyRetries(id));
            }

            sqlx::query(
                r#"
                UPDATE proof_requests
                SET proof_status = $1,
                    job_status = $2,
                    retry_count = retry_count + 1,
                    failure_reason = NULL,
                    proof_data = NULL,
                    finished_at = NULL,
                    worker_id = NULL,
                    lock_id = NULL,
                    lock_expires_at = NULL,
                    attempt = 0
                WHERE proof_id = $3
                "#,
            )
            .bind(ProofStatus::Created.as_str())
            .bind(ProofJobStatus::Pending.as_str())
            .bind(&proof_id)
            .execute(&mut *tx)
            .await
            .map_err(request_db)?;

            tx.commit().await.map_err(request_db)?;
            Ok(id)
        } else {
            // this proof request already exists in the db with a non-failing status.
            // Rollback the db transaction and return the proof id.
            tx.rollback().await.map_err(request_db)?;
            Ok(id)
        }
    }

    pub(crate) async fn proof_status(
        &self,
        proof_id: ProofRequestId,
    ) -> Result<ProofStatus, ProofRequestError> {
        let row = sqlx::query("SELECT proof_status FROM proof_requests WHERE proof_id = $1")
            .bind(proof_id_bytes(proof_id))
            .fetch_optional(&self.pool)
            .await
            .map_err(request_db)?
            .ok_or(ProofRequestError::NotFound(proof_id))?;
        parse_status(row.try_get("proof_status").map_err(request_db)?)
    }

    pub(crate) async fn get_proof(
        &self,
        proof_id: ProofRequestId,
    ) -> Result<ProofResponse, ProofRequestError> {
        let row = sqlx::query(
            "SELECT proof_status, proof_data, failure_reason FROM proof_requests WHERE proof_id = $1",
        )
        .bind(proof_id_bytes(proof_id))
        .fetch_optional(&self.pool)
        .await
        .map_err(request_db)?
        .ok_or(ProofRequestError::NotFound(proof_id))?;

        let status = parse_status(row.try_get("proof_status").map_err(request_db)?)?;
        match status {
            ProofStatus::Succeeded => {
                let data: Vec<u8> = row
                    .try_get("proof_data")
                    .map_err(|err| ProofRequestError::Internal(err.to_string()))?;
                let proof = serde_json::from_slice(&data)
                    .map_err(|err| ProofRequestError::Internal(err.to_string()))?;
                Ok(ProofResponse {
                    id: proof_id,
                    proof,
                })
            }
            ProofStatus::Failed => Err(ProofRequestError::Failed {
                id: proof_id,
                reason: row
                    .try_get::<Option<String>, _>("failure_reason")
                    .map_err(request_db)?
                    .unwrap_or_else(|| "proof job failed".to_string()),
            }),
            status => Err(ProofRequestError::Pending {
                id: proof_id,
                status,
            }),
        }
    }

    pub(crate) async fn get_next_proof(
        &self,
        backend: ProofBackend,
        worker_id: String,
    ) -> Result<Option<LockedProofRequest>, ProofJobQueueError> {
        let lock_id = LockId::new();
        let now = Utc::now();
        let lock_expires_at = now + self.config.lock_timeout;
        let query = sqlx::query(
            r#"
            UPDATE proof_requests
            SET proof_status = $1,
                worker_id = $2,
                lock_id = $3,
                job_status = $4,
                attempt = attempt + 1,
                lock_expires_at = $5,
                updated_at = $6
            WHERE proof_id = (
                SELECT proof_id FROM proof_requests
                WHERE backend = $7
                    AND (
                        job_status = $8
                        OR (job_status = $9 AND lock_expires_at < $10 AND attempt < $11)
                    ) 
                ORDER BY l2_block_number ASC, created_at ASC, proof_id ASC
                FOR UPDATE SKIP LOCKED
                LIMIT 1 
            )
            RETURNING backend, game, root_claim, l2_block_number, l1_head;
            "#,
        )
        .bind(ProofStatus::Running.as_str())
        .bind(worker_id)
        .bind(lock_id.0)
        .bind(ProofJobStatus::Claimed.as_str())
        .bind(lock_expires_at)
        .bind(now)
        .bind(backend.as_str())
        .bind(ProofJobStatus::Pending.as_str())
        .bind(ProofJobStatus::Claimed.as_str())
        .bind(now)
        .bind(self.config.max_attempts as i32)
        .fetch_optional(&self.pool)
        .await
        .map_err(queue_db)?;

        if let Some(row) = query {
            let request = request_from_row(&row).map_err(ProofJobQueueError::Internal)?;
            Ok(Some(LockedProofRequest { request, lock_id }))
        } else {
            Ok(None)
        }
    }

    pub(crate) async fn get_proof_session(
        &self,
        proof_id: ProofRequestId,
        session_type: SessionType,
    ) -> Result<Option<BackendSession>, ProofJobQueueError> {
        let row = sqlx::query(
            r#"
            SELECT ps.backend_session_id, ps.status
            FROM proof_sessions ps
            JOIN proof_requests pr ON pr.proof_id = ps.proof_id
            WHERE pr.proof_id = $1
              AND ps.session_type = $2
              AND (ps.status = $3 OR ps.status = $4)
            "#,
        )
        .bind(proof_id_bytes(proof_id))
        .bind(session_type.as_str())
        .bind(BackendSessionStatus::Submitting.as_str())
        .bind(BackendSessionStatus::Running.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(queue_db)?;

        let session = if let Some(row) = row {
            let backend_session_id: String = row.try_get("backend_session_id").map_err(queue_db)?;
            let status_str: String = row.try_get("status").map_err(queue_db)?;
            let state = BackendSessionStatus::try_from(status_str.as_str()).map_err(|e| {
                ProofJobQueueError::Internal(format!("unknown proof_sessions.status value {e:?}"))
            })?;

            Some(BackendSession {
                backend_session_id,
                status: state,
            })
        } else {
            None
        };

        Ok(session)
    }

    pub(crate) async fn record_proof_session(
        &self,
        proof_id: ProofRequestId,
        session_type: SessionType,
        worker_id: String,
        lock_id: LockId,
        backend_session_id: String,
        status: BackendSessionStatus,
    ) -> Result<(), ProofJobQueueError> {
        let now = Utc::now();
        let mut tx = self.begin_queue_tx().await?;
        let claim = sqlx::query(
            r#"
           SELECT proof_id, job_status, worker_id, lock_id, lock_expires_at
           FROM proof_requests
           WHERE proof_id = $1
           FOR UPDATE
           "#,
        )
        .bind(proof_id_bytes(proof_id))
        .fetch_optional(&mut *tx)
        .await
        .map_err(queue_db)?;

        let Some(claim) = claim else {
            return Err(ProofJobQueueError::NotFound(proof_id));
        };

        let stored_job_status_str: &str = claim.get("job_status");
        let stored_job_status = ProofJobStatus::try_from(stored_job_status_str).map_err(|e| {
            ProofJobQueueError::Internal(format!(
                "Unknown job_status '{stored_job_status_str}': {e}"
            ))
        })?;

        let stored_lock_id: Option<Uuid> = claim.get("lock_id");
        let stored_worker_id: Option<String> = claim.get("worker_id");
        let stored_lock_expires_at: Option<chrono::DateTime<Utc>> = claim.get("lock_expires_at");

        // caller must be authorized:
        // - proof status must be `Claimed`
        // - lock_id and worker_id must match
        // - lock_expires_at shoult not be expired
        if stored_job_status != ProofJobStatus::Claimed {
            return Err(ProofJobQueueError::Internal(format!(
                "Invalid status for proof {proof_id}: expected Queued, got {stored_job_status:?}"
            )));
        }
        if stored_lock_id != Some(lock_id.0) || stored_worker_id != Some(worker_id.clone()) {
            return Err(ProofJobQueueError::Internal(format!(
                "Not authorized for proof {proof_id}: lock_id or worker_id mismatch"
            )));
        }
        if stored_lock_expires_at.is_none()
            || stored_lock_expires_at
                .as_ref()
                .is_none_or(|expires_at| *expires_at <= now)
        {
            return Err(ProofJobQueueError::Internal(format!(
                "Lock expired for proof {proof_id}"
            )));
        }

        let existing_backend_sessions = sqlx::query(
            r#"
                SELECT status
                FROM proof_sessions
                WHERE proof_id = $1
                  AND session_type = $2
                  AND backend_session_id = $3
                FOR UPDATE
            "#,
        )
        .bind(proof_id_bytes(proof_id))
        .bind(session_type.as_str())
        .bind(backend_session_id.clone())
        .fetch_all(&mut *tx)
        .await
        .map_err(queue_db)?;

        for row in existing_backend_sessions {
            let status_str: &str = row.get("status");
            let status = BackendSessionStatus::try_from(status_str).map_err(|err| {
                ProofJobQueueError::Internal(format!(
                    "invalid session status '{status_str}': {err}"
                ))
            })?;
            if status.is_terminal() {
                // return this proof session
                // TODO: do it
            }
        }

        // The proof request row lock serializes worker writers, while this
        // session row lock prevents pollers from terminalizing the selected row
        // before the update below.
        let active_id: Option<i64> = sqlx::query(
            r#"
            SELECT id
            FROM proof_sessions
            WHERE proof_id = $1
              AND session_type = $2
              AND (status = $3 OR status = $4)
            FOR UPDATE
            "#,
        )
        .bind(proof_id_bytes(proof_id))
        .bind(session_type.as_str())
        .bind(BackendSessionStatus::Submitting.as_str())
        .bind(BackendSessionStatus::Running.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(queue_db)?
        .map(|r| r.get("id"));

        let _row = if let Some(active_id) = active_id {
            sqlx::query(
                r#"
                UPDATE proof_sessions
                SET backend_session_id = $1,
                    status = $2,
                    failure_reason = $3
                WHERE id = $4
                AND (status = $5 OR status = $6)
                RETURNING id
                "#,
            )
            .bind(backend_session_id)
            .bind(status.as_str())
            .bind("failure reason".to_string()) // TODO: replace this with an input value
            .bind(active_id)
            .bind(BackendSessionStatus::Submitting.as_str())
            .bind(BackendSessionStatus::Running.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(queue_db)?
            .ok_or_else(|| {
                ProofJobQueueError::Internal(
                    "active proof session status changed between SELECT FOR UPDATE and UPDATE"
                        .to_string(),
                )
            })?
        } else {
            sqlx::query(
                r#"
                INSERT INTO proof_sessions (
                    proof_id, session_type, backend_session_id, status,
                    created_at, failure_reason, completed_at
                    )
                VALUES (
                    $1, $2, $3, $4, $5, NULL, NULL
                )
                RETURNING id
                "#,
            )
            .bind(proof_id_bytes(proof_id))
            .bind(session_type.as_str())
            .bind(backend_session_id)
            .bind(status.as_str())
            .bind(now)
            .fetch_one(&mut *tx)
            .await
            .map_err(queue_db)?
        };

        tx.commit().await.map_err(queue_db)?;

        Ok(())
    }

    pub(crate) async fn submit_proof(
        &self,
        proof: ProofResponse,
        worker_id: String,
        lock_id: LockId,
    ) -> Result<(), ProofJobQueueError> {
        let now = Utc::now();
        let row = sqlx::query(
            r#"
            SELECT backend, game, root_claim, l2_block_number, l1_head, proof_status,
                lock_id, worker_id, lock_expires_at, job_status
            FROM proof_requests
            WHERE proof_id = $1
            "#,
        )
        .bind(proof_id_bytes(proof.id))
        .fetch_optional(&self.pool)
        .await
        .map_err(queue_db)?;
        let Some(existing) = row else {
            return Err(ProofJobQueueError::NotFound(proof.id));
        };
        let stored_proof_request =
            request_from_row(&existing).map_err(ProofJobQueueError::Internal)?;
        let stored_lock_id: Option<Uuid> = existing.get("lock_id");
        let stored_worker_id: Option<String> = existing.get("worker_id");
        let stored_lock_expires_at: Option<chrono::DateTime<Utc>> = existing.get("lock_expires_at");
        let stored_job_status_str: String = existing.get("job_status");
        let stored_job_status = ProofJobStatus::try_from(stored_job_status_str.as_str())
            .map_err(ProofJobQueueError::Internal)?;
        // validation
        if stored_proof_request.backend != proof.proof.backend() {
            return Err(ProofJobQueueError::Validation(format!(
                "stored backend is {} but provided proof is for {}",
                stored_proof_request.backend,
                proof.proof.backend()
            )));
        }
        if stored_lock_id != Some(lock_id.0) || stored_worker_id != Some(worker_id.clone()) {
            return Err(ProofJobQueueError::StaleLocked);
        }
        if stored_lock_expires_at.is_none()
            || stored_lock_expires_at
                .as_ref()
                .is_none_or(|expires_at| *expires_at <= now)
        {
            return Err(ProofJobQueueError::Validation(format!(
                "Lock expired for proof {}",
                stored_proof_request.id()
            )));
        }
        if matches!(
            stored_job_status,
            ProofJobStatus::Succeeded | ProofJobStatus::Failed
        ) {
            return Err(ProofJobQueueError::Validation(
                "The proof has alredy reached a terminal job status.".to_string(),
            ));
        }
        // now update the table with the proof
        let proof_data = serde_json::to_vec(&proof.proof)
            .map_err(|err| ProofJobQueueError::Internal(err.to_string()))?;
        let row = sqlx::query(
            r#"
            UPDATE proof_requests
            SET proof_status = $1,
                job_status = $2,
                proof_data = $3,
                failure_reason = NULL,
                finished_at = $4
            WHERE proof_id = $5
                AND job_status = $6
                AND worker_id = $7   
                AND lock_id = $8
                AND lock_expires_at > $9
            RETURNING proof_id
            "#,
        )
        .bind(ProofStatus::Succeeded.as_str())
        .bind(ProofJobStatus::Succeeded.as_str())
        .bind(proof_data)
        .bind(now)
        .bind(proof_id_bytes(proof.id))
        .bind(ProofJobStatus::Claimed.as_str())
        .bind(worker_id)
        .bind(lock_id.0)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(queue_db)?;

        if let Some(_row) = row {
            // db is updated, return successfully
            Ok(())
        } else {
            // TODO: to do a better analysis of the error, we should try to
            // read the row again (equal to start of this fn) and perform
            // validation again. This makes the error more clear and callers
            // of this fn may handle the specific error better.
            Err(ProofJobQueueError::Validation(
                "submit_proof is failing because there is no row to update!".to_string(),
            ))
        }
    }

    async fn begin_tx(
        &self,
        isolation_level: PostgresIsolationLevel,
    ) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(isolation_level.set_transaction_sql())
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }

    async fn begin_request_tx(&self) -> Result<Transaction<'_, Postgres>, ProofRequestError> {
        self.begin_tx(PostgresIsolationLevel::ReadCommitted)
            .await
            .map_err(request_db)
    }

    async fn begin_queue_tx(&self) -> Result<Transaction<'_, Postgres>, ProofJobQueueError> {
        self.begin_tx(PostgresIsolationLevel::ReadCommitted)
            .await
            .map_err(queue_db)
    }
}

fn proof_id_bytes(id: ProofRequestId) -> Vec<u8> {
    id.0.as_slice().to_vec()
}

fn b256_from_bytes(field: &str, bytes: Vec<u8>) -> Result<B256, String> {
    if bytes.len() != 32 {
        return Err(format!("{field} has {} bytes, expected 32", bytes.len()));
    }
    Ok(B256::from_slice(&bytes))
}

fn address_from_bytes(field: &str, bytes: Vec<u8>) -> Result<Address, String> {
    if bytes.len() != 20 {
        return Err(format!("{field} has {} bytes, expected 20", bytes.len()));
    }
    Ok(Address::from_slice(&bytes))
}

fn request_from_row(row: &PgRow) -> Result<ProofRequest, String> {
    let l2_block_number: i64 = row
        .try_get("l2_block_number")
        .map_err(|err| err.to_string())?;
    if l2_block_number < 0 {
        return Err(format!("l2_block_number is negative: {l2_block_number}"));
    }

    Ok(ProofRequest {
        backend: ProofBackend::try_from(
            row.try_get::<String, _>("backend")
                .map_err(|err| err.to_string())?
                .as_str(),
        )?,
        game: address_from_bytes("game", row.try_get("game").map_err(|err| err.to_string())?)?,
        root_claim: b256_from_bytes(
            "root_claim",
            row.try_get("root_claim").map_err(|err| err.to_string())?,
        )?,
        l2_block_number: l2_block_number as u64,
        l1_head: b256_from_bytes(
            "l1_head",
            row.try_get("l1_head").map_err(|err| err.to_string())?,
        )?,
    })
}

fn parse_status(status: String) -> Result<ProofStatus, ProofRequestError> {
    ProofStatus::try_from(status.as_str()).map_err(ProofRequestError::Internal)
}

fn l2_to_i64(value: u64) -> Result<i64, ProofRequestError> {
    i64::try_from(value)
        .map_err(|_| ProofRequestError::Internal(format!("l2 block number {value} exceeds i64")))
}

fn request_db(err: sqlx::Error) -> ProofRequestError {
    ProofRequestError::Internal(err.to_string())
}

fn queue_db(err: sqlx::Error) -> ProofJobQueueError {
    ProofJobQueueError::Internal(err.to_string())
}
