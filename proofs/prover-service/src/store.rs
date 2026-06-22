use crate::{
    config::ProverServiceConfig,
    error::{InvalidConfigError, ProofJobQueueError, ProofRequestError, ProverServiceInitError},
    types::{
        BackendProofId, BackendProofPhase, BackendProofState, BackendProofWork, LeaseToken,
        LeasedBackendProofWork, LeasedProofRequest, ProofBackend, ProofData, ProofRequest,
        ProofRequestId, ProofResponse, ProofStatus,
    },
};
use alloy_primitives::{Address, B256};
use sqlx::{
    PgPool, Postgres, Row, Transaction,
    migrate::{MigrateError, Migrator},
    postgres::{PgPoolOptions, PgRow},
};
use std::{str::FromStr, time::Duration};
use tracing::{debug, info, warn};

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, Clone)]
pub(crate) struct ProverServiceStore {
    config: ProverServiceConfig,
    pool: PgPool,
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
        let mut tx = self.begin_request_tx().await?;

        if let Some(row) =
            sqlx::query("select status from proof_jobs where proof_id = $1 for update")
                .bind(&proof_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(request_db)?
        {
            let status = parse_status(row.try_get("status").map_err(request_db)?)?;
            if status == ProofStatus::Failed {
                sqlx::query("delete from proof_backend_jobs where proof_id = $1")
                    .bind(&proof_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(request_db)?;
                sqlx::query(
                    "update proof_jobs
                     set status = 'queued',
                         proof_data = null,
                         failure_reason = null,
                         start_attempts = 0,
                         locked_until = null,
                         lease_token = null,
                         updated_at = now(),
                         finished_at = null
                     where proof_id = $1",
                )
                .bind(&proof_id)
                .execute(&mut *tx)
                .await
                .map_err(request_db)?;
                info!(%id, %backend, "failed proof request re-queued");
            }
            tx.commit().await.map_err(request_db)?;
            return Ok(id);
        }

        let queued: i64 = sqlx::query_scalar(
            "select count(*) from proof_jobs where backend = $1 and status = 'queued'",
        )
        .bind(backend.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(request_db)?;
        if queued as usize >= self.config.max_queue_len {
            return Err(ProofRequestError::QueueFull(backend));
        }

        sqlx::query(
            "insert into proof_jobs (
                proof_id, backend, game, root_claim, l2_block_number, l1_head,
                status, created_at, updated_at
             )
             values ($1, $2, $3, $4, $5, $6, 'queued', now(), now())",
        )
        .bind(&proof_id)
        .bind(backend.as_str())
        .bind(proof_request.game.as_slice())
        .bind(proof_request.root_claim.as_slice())
        .bind(l2_to_i64(proof_request.l2_block_number)?)
        .bind(proof_request.l1_head.as_slice())
        .execute(&mut *tx)
        .await
        .map_err(request_db)?;

        tx.commit().await.map_err(request_db)?;
        info!(%id, %backend, "proof request queued");
        Ok(id)
    }

    pub(crate) async fn proof_status(
        &self,
        proof_id: ProofRequestId,
    ) -> Result<ProofStatus, ProofRequestError> {
        let row = sqlx::query("select status from proof_jobs where proof_id = $1")
            .bind(proof_id_bytes(proof_id))
            .fetch_optional(&self.pool)
            .await
            .map_err(request_db)?
            .ok_or(ProofRequestError::NotFound(proof_id))?;
        parse_status(row.try_get("status").map_err(request_db)?)
    }

    pub(crate) async fn get_proof(
        &self,
        proof_id: ProofRequestId,
    ) -> Result<ProofResponse, ProofRequestError> {
        let row = sqlx::query(
            "select status, proof_data, failure_reason from proof_jobs where proof_id = $1",
        )
        .bind(proof_id_bytes(proof_id))
        .fetch_optional(&self.pool)
        .await
        .map_err(request_db)?
        .ok_or(ProofRequestError::NotFound(proof_id))?;

        let status = parse_status(row.try_get("status").map_err(request_db)?)?;
        match status {
            ProofStatus::Completed => {
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
    ) -> Result<Option<LeasedProofRequest>, ProofJobQueueError> {
        let mut tx = self.begin_queue_tx().await?;
        loop {
            let Some(row) = sqlx::query(
                "select proof_id, backend, game, root_claim, l2_block_number, l1_head,
                        start_attempts
                 from proof_jobs
                 where backend = $1
                   and (
                     status = 'queued'
                     or (status = 'starting' and locked_until < now())
                   )
                 order by created_at
                 for update skip locked
                 limit 1",
            )
            .bind(backend.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(queue_db)?
            else {
                tx.commit().await.map_err(queue_db)?;
                return Ok(None);
            };

            let proof_id = proof_id_from_row(&row).map_err(ProofJobQueueError::Internal)?;
            let attempts: i32 = row.try_get("start_attempts").map_err(queue_db)?;
            if attempts as u32 >= self.config.max_attempts {
                let reason = format!("start lease expired after {attempts} attempts");
                mark_proof_failed(&mut tx, proof_id, &reason)
                    .await
                    .map_err(queue_db)?;
                warn!(%proof_id, attempts, "proof job failed: start attempts exhausted");
                continue;
            }

            let request = request_from_row(&row).map_err(ProofJobQueueError::Internal)?;
            let lease_token = LeaseToken::new();
            sqlx::query(
                "update proof_jobs
                 set status = 'starting',
                     locked_until = now() + ($2::bigint * interval '1 millisecond'),
                     lease_token = $3,
                     start_attempts = start_attempts + 1,
                     updated_at = now()
                 where proof_id = $1",
            )
            .bind(proof_id_bytes(proof_id))
            .bind(duration_millis(self.config.lease_timeout))
            .bind(lease_token.0)
            .execute(&mut *tx)
            .await
            .map_err(queue_db)?;

            tx.commit().await.map_err(queue_db)?;
            debug!(%proof_id, %backend, attempts = attempts + 1, "proof job leased");
            return Ok(Some(LeasedProofRequest {
                request,
                lease_token,
            }));
        }
    }

    pub(crate) async fn submit_backend_proof_state(
        &self,
        proof_id: ProofRequestId,
        backend_proof_state: BackendProofState,
        lease_token: LeaseToken,
    ) -> Result<(), ProofJobQueueError> {
        let mut tx = self.begin_queue_tx().await?;
        let backend: Option<String> = sqlx::query_scalar(
            "update proof_jobs
             set status = 'backend_pending',
                 locked_until = null,
                 lease_token = null,
                 updated_at = now()
             where proof_id = $1
               and lease_token = $2
               and status = 'starting'
             returning backend",
        )
        .bind(proof_id_bytes(proof_id))
        .bind(lease_token.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(queue_db)?;

        let Some(backend) = backend else {
            return Err(classify_proof_update(&mut tx, proof_id).await);
        };

        sqlx::query(
            "insert into proof_backend_jobs (
                proof_id, backend, phase, backend_proof_id, status,
                next_poll_at, created_at, updated_at
             )
             values ($1, $2, $3, $4, 'requested', now(), now(), now())",
        )
        .bind(proof_id_bytes(proof_id))
        .bind(backend)
        .bind(backend_proof_state.phase().as_str())
        .bind(backend_proof_state.id().to_string())
        .execute(&mut *tx)
        .await
        .map_err(queue_db)?;

        tx.commit().await.map_err(queue_db)?;
        info!(%proof_id, phase = %backend_proof_state.phase(), "backend proof state submitted");
        Ok(())
    }

    pub(crate) async fn get_next_backend_proof(
        &self,
        backend: ProofBackend,
    ) -> Result<Option<LeasedBackendProofWork>, ProofJobQueueError> {
        let mut tx = self.begin_queue_tx().await?;
        loop {
            let Some(row) = sqlx::query(
                "select
                     bj.id as backend_job_id,
                     bj.phase,
                     bj.backend_proof_id,
                     bj.advance_attempts,
                     pj.proof_id,
                     pj.backend,
                     pj.game,
                     pj.root_claim,
                     pj.l2_block_number,
                     pj.l1_head
                 from proof_backend_jobs bj
                 join proof_jobs pj on pj.proof_id = bj.proof_id
                 where bj.backend = $1
                   and bj.status = 'requested'
                   and bj.next_poll_at <= now()
                   and (bj.locked_until is null or bj.locked_until < now())
                 order by bj.next_poll_at
                 for update of bj skip locked
                 limit 1",
            )
            .bind(backend.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(queue_db)?
            else {
                tx.commit().await.map_err(queue_db)?;
                return Ok(None);
            };

            let backend_job_id: i64 = row.try_get("backend_job_id").map_err(queue_db)?;
            let proof_id = proof_id_from_row(&row).map_err(ProofJobQueueError::Internal)?;
            let attempts: i32 = row.try_get("advance_attempts").map_err(queue_db)?;
            if attempts as u32 >= self.config.max_attempts {
                let reason = format!("backend lease expired after {attempts} attempts");
                mark_backend_failed(&mut tx, backend_job_id, proof_id, &reason)
                    .await
                    .map_err(queue_db)?;
                warn!(%proof_id, backend_job_id, attempts, "backend proof job failed: attempts exhausted");
                continue;
            }

            let request = request_from_row(&row).map_err(ProofJobQueueError::Internal)?;
            let phase: String = row.try_get("phase").map_err(queue_db)?;
            let phase = BackendProofPhase::try_from(phase.as_str())
                .map_err(ProofJobQueueError::Internal)?;
            let backend_proof_id: String = row.try_get("backend_proof_id").map_err(queue_db)?;
            let state = BackendProofState::from_phase(
                phase,
                BackendProofId(
                    B256::from_str(&backend_proof_id)
                        .map_err(|err| ProofJobQueueError::Internal(err.to_string()))?,
                ),
            );

            let lease_token = LeaseToken::new();
            sqlx::query(
                "update proof_backend_jobs
                 set locked_until = now() + ($2::bigint * interval '1 millisecond'),
                     lease_token = $3,
                     advance_attempts = advance_attempts + 1,
                     updated_at = now()
                 where id = $1",
            )
            .bind(backend_job_id)
            .bind(duration_millis(self.config.lease_timeout))
            .bind(lease_token.0)
            .execute(&mut *tx)
            .await
            .map_err(queue_db)?;

            tx.commit().await.map_err(queue_db)?;
            debug!(%proof_id, backend_job_id, %backend, attempts = attempts + 1, "backend proof job leased");
            return Ok(Some(LeasedBackendProofWork {
                backend_job_id,
                work: BackendProofWork {
                    proof_request: request,
                    state,
                },
                lease_token,
            }));
        }
    }

    pub(crate) async fn fail_proof(
        &self,
        proof_id: ProofRequestId,
        reason: String,
        lease_token: LeaseToken,
    ) -> Result<(), ProofJobQueueError> {
        let mut tx = self.begin_queue_tx().await?;
        let row = sqlx::query(
            "select start_attempts, backend
             from proof_jobs
             where proof_id = $1
               and lease_token = $2
               and status = 'starting'
             for update",
        )
        .bind(proof_id_bytes(proof_id))
        .bind(lease_token.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(queue_db)?;

        let Some(row) = row else {
            return Err(classify_proof_update(&mut tx, proof_id).await);
        };

        let attempts: i32 = row.try_get("start_attempts").map_err(queue_db)?;
        if attempts as u32 >= self.config.max_attempts {
            mark_proof_failed(&mut tx, proof_id, &reason)
                .await
                .map_err(queue_db)?;
            warn!(%proof_id, attempts, %reason, "proof job failed");
        } else {
            sqlx::query(
                "update proof_jobs
                 set status = 'queued',
                     locked_until = null,
                     lease_token = null,
                     updated_at = now()
                 where proof_id = $1",
            )
            .bind(proof_id_bytes(proof_id))
            .execute(&mut *tx)
            .await
            .map_err(queue_db)?;
            debug!(%proof_id, attempts, %reason, "proof attempt failed, re-queueing");
        }

        tx.commit().await.map_err(queue_db)?;
        Ok(())
    }

    pub(crate) async fn noop_backend_job(
        &self,
        backend_job_id: i64,
        lease_token: LeaseToken,
    ) -> Result<(), ProofJobQueueError> {
        let mut tx = self.begin_queue_tx().await?;
        let result = sqlx::query(
            "update proof_backend_jobs
             set locked_until = null,
                 lease_token = null,
                 next_poll_at = now() + ($3::bigint * interval '1 millisecond'),
                 updated_at = now()
             where id = $1
               and lease_token = $2
               and status = 'requested'",
        )
        .bind(backend_job_id)
        .bind(lease_token.0)
        .bind(duration_millis(self.config.backend_poll_interval))
        .execute(&mut *tx)
        .await
        .map_err(queue_db)?;

        if result.rows_affected() == 0 {
            return Err(classify_backend_update(&mut tx, backend_job_id).await);
        }
        tx.commit().await.map_err(queue_db)?;
        Ok(())
    }

    pub(crate) async fn advance_backend_job(
        &self,
        backend_job_id: i64,
        lease_token: LeaseToken,
        state: BackendProofState,
    ) -> Result<(), ProofJobQueueError> {
        let mut tx = self.begin_queue_tx().await?;
        let row = sqlx::query(
            "update proof_backend_jobs
             set status = 'completed',
                 locked_until = null,
                 lease_token = null,
                 updated_at = now(),
                 completed_at = now()
             where id = $1
               and lease_token = $2
               and status = 'requested'
             returning proof_id, backend",
        )
        .bind(backend_job_id)
        .bind(lease_token.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(queue_db)?;

        let Some(row) = row else {
            return Err(classify_backend_update(&mut tx, backend_job_id).await);
        };
        let proof_id: Vec<u8> = row.try_get("proof_id").map_err(queue_db)?;
        let backend: String = row.try_get("backend").map_err(queue_db)?;

        sqlx::query(
            "insert into proof_backend_jobs (
                proof_id, backend, phase, backend_proof_id, status,
                next_poll_at, created_at, updated_at
             )
             values ($1, $2, $3, $4, 'requested', now(), now(), now())",
        )
        .bind(proof_id)
        .bind(backend)
        .bind(state.phase().as_str())
        .bind(state.id().to_string())
        .execute(&mut *tx)
        .await
        .map_err(queue_db)?;

        tx.commit().await.map_err(queue_db)?;
        Ok(())
    }

    pub(crate) async fn fail_backend_proof_job(
        &self,
        backend_job_id: i64,
        reason: String,
        lease_token: LeaseToken,
    ) -> Result<(), ProofJobQueueError> {
        let mut tx = self.begin_queue_tx().await?;
        let row = sqlx::query(
            "select proof_id, advance_attempts
             from proof_backend_jobs
             where id = $1
               and lease_token = $2
               and status = 'requested'
             for update",
        )
        .bind(backend_job_id)
        .bind(lease_token.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(queue_db)?;

        let Some(row) = row else {
            return Err(classify_backend_update(&mut tx, backend_job_id).await);
        };
        let proof_id = proof_id_from_bytes(row.try_get("proof_id").map_err(queue_db)?)
            .map_err(ProofJobQueueError::Internal)?;
        let attempts: i32 = row.try_get("advance_attempts").map_err(queue_db)?;
        if attempts as u32 >= self.config.max_attempts {
            mark_backend_failed(&mut tx, backend_job_id, proof_id, &reason)
                .await
                .map_err(queue_db)?;
            warn!(%proof_id, backend_job_id, attempts, %reason, "backend proof job failed");
        } else {
            sqlx::query(
                "update proof_backend_jobs
                 set locked_until = null,
                     lease_token = null,
                     next_poll_at = now() + ($3::bigint * interval '1 millisecond'),
                     updated_at = now()
                 where id = $1
                   and lease_token = $2
                   and status = 'requested'",
            )
            .bind(backend_job_id)
            .bind(lease_token.0)
            .bind(duration_millis(self.config.backend_poll_interval))
            .execute(&mut *tx)
            .await
            .map_err(queue_db)?;
            debug!(%proof_id, backend_job_id, attempts, %reason, "backend proof attempt failed, re-scheduling");
        }

        tx.commit().await.map_err(queue_db)?;
        Ok(())
    }

    pub(crate) async fn fail_backend_job(
        &self,
        backend_job_id: i64,
        lease_token: LeaseToken,
        reason: &str,
    ) -> Result<(), ProofJobQueueError> {
        let mut tx = self.begin_queue_tx().await?;
        let row = sqlx::query(
            "update proof_backend_jobs
             set status = 'failed',
                 failure_reason = $3,
                 locked_until = null,
                 lease_token = null,
                 updated_at = now(),
                 completed_at = now()
             where id = $1
               and lease_token = $2
               and status = 'requested'
             returning proof_id",
        )
        .bind(backend_job_id)
        .bind(lease_token.0)
        .bind(reason)
        .fetch_optional(&mut *tx)
        .await
        .map_err(queue_db)?;

        let Some(row) = row else {
            return Err(classify_backend_update(&mut tx, backend_job_id).await);
        };
        let proof_id = proof_id_from_bytes(row.try_get("proof_id").map_err(queue_db)?)
            .map_err(ProofJobQueueError::Internal)?;
        mark_proof_failed(&mut tx, proof_id, reason)
            .await
            .map_err(queue_db)?;
        tx.commit().await.map_err(queue_db)?;
        Ok(())
    }

    pub(crate) async fn submit_completed_backend_proof(
        &self,
        backend_job_id: i64,
        lease_token: LeaseToken,
        proof: ProofData,
    ) -> Result<(), ProofJobQueueError> {
        let proof_id = self
            .backend_job_proof_id(backend_job_id, lease_token)
            .await?;
        self.submit_proof_from_backend_job(
            ProofResponse {
                id: proof_id,
                proof,
            },
            backend_job_id,
            lease_token,
        )
        .await
    }

    async fn backend_job_proof_id(
        &self,
        backend_job_id: i64,
        lease_token: LeaseToken,
    ) -> Result<ProofRequestId, ProofJobQueueError> {
        let mut tx = self.begin_queue_tx().await?;
        let row = sqlx::query(
            "select proof_id
             from proof_backend_jobs
             where id = $1
               and lease_token = $2
               and status = 'requested'",
        )
        .bind(backend_job_id)
        .bind(lease_token.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(queue_db)?;
        let Some(row) = row else {
            return Err(classify_backend_update(&mut tx, backend_job_id).await);
        };
        let proof_id = proof_id_from_bytes(row.try_get("proof_id").map_err(queue_db)?)
            .map_err(ProofJobQueueError::Internal)?;
        tx.commit().await.map_err(queue_db)?;
        Ok(proof_id)
    }

    pub(crate) async fn submit_proof_from_proof_job(
        &self,
        proof: ProofResponse,
        lease_token: LeaseToken,
    ) -> Result<(), ProofJobQueueError> {
        let mut tx = self.begin_queue_tx().await?;
        let row = sqlx::query(
            "select backend
             from proof_jobs
             where proof_id = $1
               and lease_token = $2
               and status = 'starting'
             for update",
        )
        .bind(proof_id_bytes(proof.id))
        .bind(lease_token.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(queue_db)?;

        let Some(row) = row else {
            return Err(classify_proof_update(&mut tx, proof.id).await);
        };
        let expected = parse_backend(row.try_get("backend").map_err(queue_db)?)?;
        validate_proof_backend(proof.id, &proof.proof, expected)?;
        complete_proof(&mut tx, proof).await.map_err(queue_db)?;
        tx.commit().await.map_err(queue_db)?;
        Ok(())
    }

    pub(crate) async fn submit_proof_from_backend_job(
        &self,
        proof: ProofResponse,
        backend_job_id: i64,
        lease_token: LeaseToken,
    ) -> Result<(), ProofJobQueueError> {
        let mut tx = self.begin_queue_tx().await?;
        let row = sqlx::query(
            "select bj.proof_id, pj.backend
             from proof_backend_jobs bj
             join proof_jobs pj on pj.proof_id = bj.proof_id
             where bj.id = $1
               and bj.lease_token = $2
               and bj.status = 'requested'
             for update of bj, pj",
        )
        .bind(backend_job_id)
        .bind(lease_token.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(queue_db)?;

        let Some(row) = row else {
            return Err(classify_backend_update(&mut tx, backend_job_id).await);
        };
        let expected_id = proof_id_from_bytes(row.try_get("proof_id").map_err(queue_db)?)
            .map_err(ProofJobQueueError::Internal)?;
        if expected_id != proof.id {
            return Err(ProofJobQueueError::InvalidProof {
                id: proof.id,
                reason: format!("backend job belongs to proof {expected_id}"),
            });
        }
        let expected_backend = parse_backend(row.try_get("backend").map_err(queue_db)?)?;
        validate_proof_backend(proof.id, &proof.proof, expected_backend)?;

        sqlx::query(
            "update proof_backend_jobs
             set status = 'completed',
                 locked_until = null,
                 lease_token = null,
                 updated_at = now(),
                 completed_at = now()
             where id = $1",
        )
        .bind(backend_job_id)
        .execute(&mut *tx)
        .await
        .map_err(queue_db)?;
        complete_proof(&mut tx, proof).await.map_err(queue_db)?;
        tx.commit().await.map_err(queue_db)?;
        Ok(())
    }

    async fn begin_request_tx(&self) -> Result<Transaction<'_, Postgres>, ProofRequestError> {
        self.pool.begin().await.map_err(request_db)
    }

    async fn begin_queue_tx(&self) -> Result<Transaction<'_, Postgres>, ProofJobQueueError> {
        self.pool.begin().await.map_err(queue_db)
    }
}

fn proof_id_bytes(id: ProofRequestId) -> Vec<u8> {
    id.0.as_slice().to_vec()
}

fn proof_id_from_row(row: &PgRow) -> Result<ProofRequestId, String> {
    proof_id_from_bytes(row.try_get("proof_id").map_err(|err| err.to_string())?)
}

fn proof_id_from_bytes(bytes: Vec<u8>) -> Result<ProofRequestId, String> {
    Ok(ProofRequestId(b256_from_bytes("proof_id", bytes)?))
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

fn parse_backend(backend: String) -> Result<ProofBackend, ProofJobQueueError> {
    ProofBackend::try_from(backend.as_str()).map_err(ProofJobQueueError::Internal)
}

fn l2_to_i64(value: u64) -> Result<i64, ProofRequestError> {
    i64::try_from(value)
        .map_err(|_| ProofRequestError::Internal(format!("l2 block number {value} exceeds i64")))
}

fn duration_millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn request_db(err: sqlx::Error) -> ProofRequestError {
    ProofRequestError::Internal(err.to_string())
}

fn queue_db(err: sqlx::Error) -> ProofJobQueueError {
    ProofJobQueueError::Internal(err.to_string())
}

async fn classify_proof_update(
    tx: &mut Transaction<'_, Postgres>,
    proof_id: ProofRequestId,
) -> ProofJobQueueError {
    let exists = sqlx::query_scalar::<_, bool>(
        "select exists(select 1 from proof_jobs where proof_id = $1)",
    )
    .bind(proof_id_bytes(proof_id))
    .fetch_one(&mut **tx)
    .await
    .unwrap_or(false);
    if exists {
        ProofJobQueueError::StaleLease
    } else {
        ProofJobQueueError::UnknownJob(proof_id)
    }
}

async fn classify_backend_update(
    tx: &mut Transaction<'_, Postgres>,
    backend_job_id: i64,
) -> ProofJobQueueError {
    let exists = sqlx::query_scalar::<_, bool>(
        "select exists(select 1 from proof_backend_jobs where id = $1)",
    )
    .bind(backend_job_id)
    .fetch_one(&mut **tx)
    .await
    .unwrap_or(false);
    if exists {
        ProofJobQueueError::StaleLease
    } else {
        ProofJobQueueError::UnknownBackendJob(backend_job_id)
    }
}

async fn mark_proof_failed(
    tx: &mut Transaction<'_, Postgres>,
    proof_id: ProofRequestId,
    reason: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update proof_jobs
         set status = 'failed',
             proof_data = null,
             failure_reason = $2,
             locked_until = null,
             lease_token = null,
             updated_at = now(),
             finished_at = now()
         where proof_id = $1",
    )
    .bind(proof_id_bytes(proof_id))
    .bind(reason)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn mark_backend_failed(
    tx: &mut Transaction<'_, Postgres>,
    backend_job_id: i64,
    proof_id: ProofRequestId,
    reason: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update proof_backend_jobs
         set status = 'failed',
             failure_reason = $2,
             locked_until = null,
             lease_token = null,
             updated_at = now(),
             completed_at = now()
         where id = $1",
    )
    .bind(backend_job_id)
    .bind(reason)
    .execute(&mut **tx)
    .await?;
    mark_proof_failed(tx, proof_id, reason).await
}

async fn complete_proof(
    tx: &mut Transaction<'_, Postgres>,
    proof: ProofResponse,
) -> Result<(), sqlx::Error> {
    let proof_data =
        serde_json::to_vec(&proof.proof).map_err(|err| sqlx::Error::Decode(Box::new(err)))?;
    sqlx::query(
        "update proof_jobs
         set status = 'completed',
             proof_data = $2,
             failure_reason = null,
             locked_until = null,
             lease_token = null,
             updated_at = now(),
             finished_at = now()
         where proof_id = $1",
    )
    .bind(proof_id_bytes(proof.id))
    .bind(proof_data)
    .execute(&mut **tx)
    .await?;
    info!(id = %proof.id, "proof job completed");
    Ok(())
}

fn validate_proof_backend(
    id: ProofRequestId,
    proof: &ProofData,
    expected: ProofBackend,
) -> Result<(), ProofJobQueueError> {
    if proof.backend() == expected {
        Ok(())
    } else {
        Err(ProofJobQueueError::InvalidProof {
            id,
            reason: format!(
                "backend mismatch: expected {}, got {}",
                expected,
                proof.backend()
            ),
        })
    }
}
