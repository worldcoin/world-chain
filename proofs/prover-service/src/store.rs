use crate::{
    ProofData,
    config::ProverServiceConfig,
    error::{
        BackendMismatchErrorData, BackendSessionAlreadyTerminalErrorData, InvalidConfigError,
        ProofJobQueueError, ProofJobStatusErrorData, ProofMismatchErrorData, ProofRequestError,
        ProverServiceInitError, TooManyRetriesErrorData,
    },
    types::{
        BackendSession, BackendSessionStatus, FailedProofResponse, LockId, LockedProofRequest,
        PendingProofResponse, ProofBackend, ProofJobStatus, ProofRequest, ProofRequestId,
        ProofResponse, ProofStatus, SessionType, SucceededProofResponse,
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
        .await?;

        if insert_result.rows_affected() > 0 {
            // no conflict
            tx.commit().await?;
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
        .await?;

        let Some(row) = row else {
            tx.rollback().await?;
            return Err(ProofRequestError::RowMissingAfterConflict(id));
        };

        let proof_status_str: &str = row.get("proof_status");
        let proof_status = ProofStatus::try_from(proof_status_str)
            .map_err(ProofRequestError::UnknownProofStatus)?;

        if proof_status == ProofStatus::Failed {
            // retry the entire proof job if retry_count is less than the max_retry
            let retry_count: i32 = row.get("retry_count");
            if retry_count > self.config.max_retries as i32 {
                tx.rollback().await?;
                return Err(ProofRequestError::TooManyRetries(TooManyRetriesErrorData {
                    proof_id: id,
                    max_retries: self.config.max_retries,
                }));
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
            .await?;

            tx.commit().await?;
            Ok(id)
        } else {
            // this proof request already exists in the db with a non-failing status.
            // Rollback the db transaction and return the proof id.
            tx.rollback().await?;
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
            .await?
            .ok_or(ProofRequestError::ProofIdNotFound(proof_id))?;
        parse_status(row.get("proof_status"))
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
        .await?
        .ok_or(ProofRequestError::ProofIdNotFound(proof_id))?;

        let status = parse_status(row.get("proof_status"))?;
        match status {
            ProofStatus::Succeeded => {
                let data: Vec<u8> = row.get("proof_data");
                let proof =
                    serde_json::from_slice(&data).map_err(ProofRequestError::ProofEncoding)?;
                Ok(ProofResponse::Succeeded(SucceededProofResponse {
                    id: proof_id,
                    proof,
                }))
            }
            ProofStatus::Failed => Ok(ProofResponse::Failed(FailedProofResponse {
                id: proof_id,
                reason: row
                    .get::<Option<String>, _>("failure_reason")
                    .unwrap_or_else(|| "proof job failed".to_string()),
            })),
            status => Ok(ProofResponse::Pending(PendingProofResponse {
                id: proof_id,
                status,
            })),
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
        .await?;

        if let Some(row) = query {
            let request = request_from_row(&row)?;
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
              AND ps.status IN ($3, $4, $5)
            ORDER BY
              CASE
                WHEN ps.status IN ($3, $4) THEN 0
                ELSE 1
              END,
              ps.completed_at DESC NULLS LAST,
              ps.id DESC
            LIMIT 1
            "#,
        )
        .bind(proof_id_bytes(proof_id))
        .bind(session_type.as_str())
        .bind(BackendSessionStatus::Submitting.as_str())
        .bind(BackendSessionStatus::Running.as_str())
        .bind(BackendSessionStatus::Completed.as_str())
        .fetch_optional(&self.pool)
        .await?;

        let session = if let Some(row) = row {
            let backend_session_id: String = row.try_get("backend_session_id")?;
            let status_str: String = row.try_get("status")?;
            let state = BackendSessionStatus::try_from(status_str.as_str())
                .map_err(ProofJobQueueError::UnknownBackendSessionStatus)?;

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
        failure_reason: String,
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
        .await?;

        let Some(claim) = claim else {
            return Err(ProofJobQueueError::ProofIdNotFound(proof_id));
        };

        let stored_job_status_str: &str = claim.get("job_status");
        let stored_job_status = ProofJobStatus::try_from(stored_job_status_str)
            .map_err(ProofJobQueueError::UnknownProofJobStatus)?;

        let stored_lock_id: Option<Uuid> = claim.get("lock_id");
        let stored_worker_id: Option<String> = claim.get("worker_id");
        let stored_lock_expires_at: Option<chrono::DateTime<Utc>> = claim.get("lock_expires_at");

        // caller must be authorized:
        // - proof status must be `Claimed`
        // - lock_id and worker_id must match
        // - lock_expires_at shoult not be expired
        if stored_job_status != ProofJobStatus::Claimed {
            return Err(ProofJobQueueError::ProofJobStatusNotClaimed(
                ProofJobStatusErrorData {
                    proof_id,
                    expected: ProofJobStatus::Claimed,
                    actual: stored_job_status,
                },
            ));
        }
        if stored_lock_id != Some(lock_id.0) || stored_worker_id != Some(worker_id.clone()) {
            return Err(ProofJobQueueError::StaleLock(proof_id));
        }
        if stored_lock_expires_at.is_none()
            || stored_lock_expires_at
                .as_ref()
                .is_none_or(|expires_at| *expires_at <= now)
        {
            return Err(ProofJobQueueError::LockExpired(proof_id));
        }

        let existing_backend_sessions = sqlx::query(
            r#"
                SELECT status
                FROM proof_sessions
                WHERE proof_id = $1
                  AND session_type = $2
                  AND backend_session_id = $3
                ORDER BY id
                FOR UPDATE
            "#,
        )
        .bind(proof_id_bytes(proof_id))
        .bind(session_type.as_str())
        .bind(backend_session_id.clone())
        .fetch_all(&mut *tx)
        .await?;

        // A terminal backend session is immutable, so short-circuit before the
        // active-session update below. Re-recording the same status is an idempotent
        // retry, any other status is a conflict.
        for row in &existing_backend_sessions {
            let status_str: &str = row.get("status");
            let stored = BackendSessionStatus::try_from(status_str)
                .map_err(ProofJobQueueError::UnknownBackendSessionStatus)?;
            if stored.is_terminal() {
                if stored == status {
                    return Ok(());
                }
                return Err(ProofJobQueueError::BackendSessionAlreadyTerminal(
                    BackendSessionAlreadyTerminalErrorData {
                        proof_id,
                        session_type,
                        backend_session_id,
                        stored,
                        attempted: status,
                    },
                ));
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
        .await?
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
            .bind(failure_reason)
            .bind(active_id)
            .bind(BackendSessionStatus::Submitting.as_str())
            .bind(BackendSessionStatus::Running.as_str())
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| {
                ProofJobQueueError::Sqlx(sqlx::Error::Protocol(
                    "active proof session status changed between SELECT FOR UPDATE and UPDATE"
                        .into(),
                ))
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
            .await?
        };

        tx.commit().await?;

        Ok(())
    }

    pub(crate) async fn submit_proof(
        &self,
        proof: SucceededProofResponse,
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
        .await?;
        let Some(existing) = row else {
            return Err(ProofJobQueueError::ProofIdNotFound(proof.id));
        };
        let stored_proof_request = request_from_row(&existing)?;
        let stored_lock_id: Option<Uuid> = existing.get("lock_id");
        let stored_worker_id: Option<String> = existing.get("worker_id");
        let stored_lock_expires_at: Option<chrono::DateTime<Utc>> = existing.get("lock_expires_at");
        let stored_job_status_str: String = existing.get("job_status");
        let stored_job_status = ProofJobStatus::try_from(stored_job_status_str.as_str())
            .map_err(ProofJobQueueError::UnknownProofJobStatus)?;
        // validation
        if stored_proof_request.backend != proof.proof.backend() {
            return Err(ProofJobQueueError::BackendMismatch(
                BackendMismatchErrorData {
                    proof_id: proof.id,
                    expected: stored_proof_request.backend,
                    actual: proof.proof.backend(),
                },
            ));
        }
        if stored_lock_id != Some(lock_id.0) || stored_worker_id != Some(worker_id.clone()) {
            return Err(ProofJobQueueError::StaleLock(proof.id));
        }
        if stored_lock_expires_at.is_none()
            || stored_lock_expires_at
                .as_ref()
                .is_none_or(|expires_at| *expires_at <= now)
        {
            return Err(ProofJobQueueError::LockExpired(proof.id));
        }
        if matches!(
            stored_job_status,
            ProofJobStatus::Succeeded | ProofJobStatus::Failed
        ) {
            return Err(ProofJobQueueError::AlreadyTerminal(proof.id));
        }
        // now update the table with the proof
        let proof_data = serde_json::to_vec(&proof.proof)?;
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
        .bind(worker_id.clone())
        .bind(lock_id.0)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(_row) = row {
            // db is updated, return successfully
            Ok(())
        } else {
            // re-read the row to anaylize the error or return Ok(()) if the proof
            // has already been submitted (idempotency).
            let row = sqlx::query(
                r#"
                SELECT backend, game, root_claim, l2_block_number, l1_head, proof_status,
                    lock_id, worker_id, lock_expires_at, job_status, proof_data
                FROM proof_requests
                WHERE proof_id = $1
                "#,
            )
            .bind(proof_id_bytes(proof.id))
            .fetch_optional(&self.pool)
            .await?;
            let Some(existing) = row else {
                return Err(ProofJobQueueError::ProofIdNotFound(proof.id));
            };
            let stored_lock_id: Option<Uuid> = existing.get("lock_id");
            let stored_worker_id: Option<String> = existing.get("worker_id");
            let stored_lock_expires_at: Option<chrono::DateTime<Utc>> =
                existing.get("lock_expires_at");
            let stored_job_status_str: String = existing.get("job_status");
            let stored_job_status = ProofJobStatus::try_from(stored_job_status_str.as_str())
                .map_err(ProofJobQueueError::UnknownProofJobStatus)?;
            // validation
            if stored_job_status == ProofJobStatus::Succeeded
                && stored_worker_id == Some(worker_id.clone())
                && stored_lock_id == Some(lock_id.0)
            {
                let stored_proof_data_vec: Vec<u8> = existing.get("proof_data");
                let stored_proof_data: ProofData = serde_json::from_slice(&stored_proof_data_vec)?;
                if stored_proof_data == proof.proof {
                    // proof has already been submitted - no op
                    return Ok(());
                } else {
                    return Err(ProofJobQueueError::ProofMismatch(Box::new(
                        ProofMismatchErrorData {
                            proof_id: proof.id,
                            expected: stored_proof_data,
                            actual: proof.proof,
                        },
                    )));
                }
            }
            if matches!(
                stored_job_status,
                ProofJobStatus::Succeeded | ProofJobStatus::Failed
            ) {
                return Err(ProofJobQueueError::AlreadyTerminal(proof.id));
            }
            if stored_job_status != ProofJobStatus::Claimed {
                return Err(ProofJobQueueError::ProofJobStatusNotClaimed(
                    ProofJobStatusErrorData {
                        proof_id: proof.id,
                        expected: ProofJobStatus::Claimed,
                        actual: stored_job_status,
                    },
                ));
            }
            if stored_lock_id != Some(lock_id.0) || stored_worker_id != Some(worker_id) {
                return Err(ProofJobQueueError::StaleLock(proof.id));
            }
            if stored_lock_expires_at.is_none() || stored_lock_expires_at.unwrap() <= Utc::now() {
                return Err(ProofJobQueueError::LockExpired(proof.id));
            }
            Err(ProofJobQueueError::Unknown(proof.id))
        }
    }

    pub(crate) async fn heartbeat(
        &self,
        proof_id: ProofRequestId,
        worker_id: String,
        lock_id: LockId,
    ) -> Result<(), ProofJobQueueError> {
        let now = Utc::now();
        let next_lock_expiration = now + self.config.lock_timeout;
        let maybe_row = sqlx::query(
            r#"
            UPDATE proof_requests
            SET lock_expires_at = $1,
                updated_at = $2
            WHERE proof_id = $3
                AND job_status = $4
                AND worker_id = $5
                AND lock_id = $6
                AND lock_expires_at > $7
                RETURNING proof_id
            "#,
        )
        .bind(next_lock_expiration)
        .bind(now)
        .bind(proof_id_bytes(proof_id))
        .bind(ProofJobStatus::Claimed.as_str())
        .bind(worker_id.clone())
        .bind(lock_id.0)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;

        if maybe_row.is_some() {
            // row updated, return successfully
            Ok(())
        } else {
            // read the row to return a better error
            let row = sqlx::query(
                r#"
                SELECT backend, game, root_claim, l2_block_number, l1_head, proof_status,
                    lock_id, worker_id, lock_expires_at, job_status
                FROM proof_requests
                WHERE proof_id = $1
                "#,
            )
            .bind(proof_id_bytes(proof_id))
            .fetch_optional(&self.pool)
            .await?;
            let Some(existing) = row else {
                return Err(ProofJobQueueError::ProofIdNotFound(proof_id));
            };
            // validation to return a proper error
            let stored_job_status_str: &str = existing.get("job_status");
            let stored_lock_id: Option<Uuid> = existing.try_get("lock_id").ok();
            let stored_worker_id: Option<String> = existing.get("worker_id");
            let stored_lock_expires_at: Option<chrono::DateTime<Utc>> =
                existing.get("lock_expires_at");
            let stored_parsed_status = ProofJobStatus::try_from(stored_job_status_str)
                .map_err(ProofJobQueueError::UnknownProofJobStatus)?;
            if matches!(
                stored_parsed_status,
                ProofJobStatus::Succeeded | ProofJobStatus::Failed
            ) {
                return Err(ProofJobQueueError::AlreadyTerminal(proof_id));
            }
            if stored_parsed_status != ProofJobStatus::Claimed {
                return Err(ProofJobQueueError::ProofJobStatusNotClaimed(
                    ProofJobStatusErrorData {
                        proof_id,
                        expected: ProofJobStatus::Claimed,
                        actual: stored_parsed_status,
                    },
                ));
            }
            if stored_lock_id != Some(lock_id.0) || stored_worker_id != Some(worker_id) {
                return Err(ProofJobQueueError::StaleLock(proof_id));
            }
            if stored_lock_expires_at.is_none()
                || stored_lock_expires_at.is_none_or(|expires_at| expires_at <= now)
            {
                return Err(ProofJobQueueError::LockExpired(proof_id));
            }

            Err(ProofJobQueueError::Unknown(proof_id))
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
            .map_err(ProofRequestError::Sqlx)
    }

    async fn begin_queue_tx(&self) -> Result<Transaction<'_, Postgres>, ProofJobQueueError> {
        self.begin_tx(PostgresIsolationLevel::ReadCommitted)
            .await
            .map_err(ProofJobQueueError::Sqlx)
    }
}

fn proof_id_bytes(id: ProofRequestId) -> Vec<u8> {
    id.0.as_slice().to_vec()
}

fn b256_from_bytes(bytes: Vec<u8>) -> Result<B256, ProofJobQueueError> {
    if bytes.len() != 32 {
        return Err(ProofJobQueueError::MalformedB256(bytes.len()));
    }
    Ok(B256::from_slice(&bytes))
}

fn address_from_bytes(bytes: Vec<u8>) -> Result<Address, ProofJobQueueError> {
    if bytes.len() != 20 {
        return Err(ProofJobQueueError::MalformedAddress(bytes.len()));
    }
    Ok(Address::from_slice(&bytes))
}

fn request_from_row(row: &PgRow) -> Result<ProofRequest, ProofJobQueueError> {
    let l2_block_number: i64 = row.try_get("l2_block_number")?;
    if l2_block_number < 0 {
        return Err(ProofJobQueueError::NegativeBlockNumber(l2_block_number));
    }

    Ok(ProofRequest {
        backend: ProofBackend::try_from(row.try_get::<String, _>("backend")?.as_str())
            .map_err(ProofJobQueueError::UnknownProofBackend)?,
        game: address_from_bytes(row.try_get("game")?)?,
        root_claim: b256_from_bytes(row.try_get("root_claim")?)?,
        l2_block_number: l2_block_number as u64,
        l1_head: b256_from_bytes(row.try_get("l1_head")?)?,
    })
}

fn parse_status(status: String) -> Result<ProofStatus, ProofRequestError> {
    ProofStatus::try_from(status.as_str()).map_err(ProofRequestError::UnknownProofStatus)
}

fn l2_to_i64(value: u64) -> Result<i64, ProofRequestError> {
    i64::try_from(value).map_err(|_| ProofRequestError::BlockNumberExceedsI64(value))
}
