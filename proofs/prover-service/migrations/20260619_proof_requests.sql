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

    backend             text not null,
    phase               text not null,
    backend_proof_id    text not null,

    status              text not null,
    advance_attempts    integer not null default 0,
    next_poll_at        timestamptz not null,
    lock_expires_at     timestamptz null,
    lock_id             uuid null,

    artifact            bytea null,
    failure_reason      text null,

    created_at          timestamptz not null,
    updated_at          timestamptz not null,
    completed_at        timestamptz null,

    unique (backend, backend_proof_id),
    unique (proof_id, phase),

    constraint proof_sessions_status_check
        check (status in ('requested', 'completed', 'failed')),
    constraint proof_sessions_phase_check
        check (phase in ('single', 'range', 'aggregation'))
);

create index proof_sessions_due_idx
    on proof_sessions (backend, next_poll_at)
    where status = 'requested';

create index proof_sessions_lock_idx
    on proof_sessions (lock_expires_at)
    where status = 'requested';
