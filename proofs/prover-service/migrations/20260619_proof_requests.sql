create table proof_requests (
    proof_id            bytea primary key,
    backend             text not null,
    game                bytea not null,
    root_claim          bytea not null,
    l2_block_number     bigint not null,
    l1_head             bytea not null,

    status              text not null,
    proof_data          bytea null,
    failure_reason      text null,

    worker_id           text null,
    lock_id             uuid null,

    start_attempts      integer not null default 0,
    lock_expires_at     timestamp with time zone null,

    created_at          timestamptz not null,
    updated_at          timestamptz not null,
    finished_at         timestamptz null,

    constraint proof_requests_status_check
        check (status in ('queued', 'starting', 'backend_pending', 'completed', 'failed'))
);

create index proof_requests_queued_idx
    on proof_requests (backend, created_at)
    where status = 'queued';

create index proof_requests_starting_lock_idx
    on proof_requests (lock_expires_at)
    where status = 'starting';

create table proof_sessions (
    id                  bigserial primary key,
    proof_id            bytea not null references proof_requests(proof_id),

    session_type        text not null,
    backend_session_id  text not null,

    status              text not null,

    failure_reason      text null,

    created_at          timestamptz not null,
    completed_at        timestamptz null,

    constraint proof_sessions_session_type_check
        check (session_type in ('snark', 'stark'))
);

create unique index proof_sessions_active_unique_idx
    on proof_sessions (proof_id, session_type)
    where status in ('submitting', 'running');
