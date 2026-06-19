create table proof_jobs (
    proof_id            bytea primary key,
    backend             text not null,
    game                bytea not null,
    root_claim          bytea not null,
    l2_block_number     bigint not null,
    l1_head             bytea not null,

    status              text not null,
    proof_data          bytea null,
    failure_reason      text null,

    start_attempts      integer not null default 0,
    locked_until        timestamptz null,
    lease_token         uuid null,

    created_at          timestamptz not null,
    updated_at          timestamptz not null,
    finished_at         timestamptz null,

    constraint proof_jobs_status_check
        check (status in ('queued', 'starting', 'backend_pending', 'completed', 'failed'))
);

create index proof_jobs_queued_idx
    on proof_jobs (backend, created_at)
    where status = 'queued';

create index proof_jobs_starting_lease_idx
    on proof_jobs (locked_until)
    where status = 'starting';

create table proof_backend_jobs (
    id                  bigserial primary key,
    proof_id            bytea not null references proof_jobs(proof_id),

    backend             text not null,
    phase               text not null,
    backend_proof_id    text not null,

    status              text not null,
    advance_attempts    integer not null default 0,
    next_poll_at        timestamptz not null,
    locked_until        timestamptz null,
    lease_token         uuid null,

    artifact            bytea null,
    failure_reason      text null,

    created_at          timestamptz not null,
    updated_at          timestamptz not null,
    completed_at        timestamptz null,

    unique (backend, backend_proof_id),
    unique (proof_id, phase),

    constraint proof_backend_jobs_status_check
        check (status in ('requested', 'completed', 'failed')),
    constraint proof_backend_jobs_phase_check
        check (phase in ('single', 'range', 'aggregation'))
);

create index proof_backend_jobs_due_idx
    on proof_backend_jobs (backend, next_poll_at)
    where status = 'requested';

create index proof_backend_jobs_lease_idx
    on proof_backend_jobs (locked_until)
    where status = 'requested';
