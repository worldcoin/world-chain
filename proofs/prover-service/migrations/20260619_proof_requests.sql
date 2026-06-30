CREATE TABLE proof_requests (
    proof_id            BYTEA PRIMARY KEY,
    backend             TEXT NOT NULL,
    game                BYTEA NOT NULL,
    root_claim          BYTEA NOT NULL,
    l2_block_number     BIGINT NOT NULL,
    l1_head             BYTEA NOT NULL,

    proof_status        TEXT NOT NULL,
    proof_data          BYTEA NULL,
    failure_reason      TEXT NULL,
    retry_count         INTEGER NOT NULL DEFAULT 0,

    worker_id           TEXT NULL,
    lock_id             UUID NULL,
    job_status          TEXT NOT NULL,
    attempt             INTEGER NOT NULL DEFAULT 0,
    lock_expires_at     TIMESTAMP WITH TIME ZONE NULL,

    created_at          TIMESTAMPTZ NOT NULL,
    updated_at          TIMESTAMPTZ NOT NULL,
    finished_at         TIMESTAMPTZ NULL,

    CONSTRAINT proof_requests_status_check
        CHECK (proof_status IN ('CREATED', 'RUNNING', 'SUCCEEDED', 'FAILED')),
    CONSTRAINT proof_requests_job_status_check
        CHECK (job_status IN ('PENDING', 'CLAIMED', 'SUCCEEDED', 'FAILED'))
   
);

CREATE INDEX proof_requests_queued_idx
    ON proof_requests (backend, created_at)
    WHERE status = 'queued';

CREATE INDEX proof_requests_starting_lock_idx
    ON proof_requests (lock_expires_at)
    WHERE status = 'starting';

CREATE TABLE proof_sessions (
    id                  BIGSERIAL PRIMARY KEY,
    proof_id            BYTEA NOT NULL REFERENCES proof_requests(proof_id),

    session_type        TEXT NOT NULL,
    backend_session_id  TEXT NOT NULL,

    status              TEXT NOT NULL,

    failure_reason      TEXT NULL,

    created_at          TIMESTAMPTZ NOT NULL,
    completed_at        TIMESTAMPTZ NULL,

    CONSTRAINT proof_sessions_session_type_check
        CHECK (session_type IN ('SNARK', 'STARK'))
);

CREATE UNIQUE INDEX proof_sessions_active_unique_idx
    ON proof_sessions (proof_id, session_type)
    WHERE status IN ('SUBMITTING', 'RUNNING');
