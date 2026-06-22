# `prover-service` Postgres workflow

This document describes the proposed persistent `prover-service` state model and the worker API
changes needed to support long-running external proof backends.

The initial simplifying assumption is that SP1 creates exactly one range proof for the whole block
range, and then one aggregation proof. If range proof splitting is reintroduced later, the backend
job table can be extended with a `phase_index` column.

## Goals

- Persist proof request state in Postgres instead of keeping it in memory.
- Allow SP1 workers to request work from the SP1 network, persist the returned SP1 request id, and
  recover from worker restarts without re-requesting already-started SP1 work.
- Keep Nitro and SP1 behind the same generic `ProofWorker`.
- Keep SP1-specific range and aggregation logic inside the SP1 backend, not inside the
  `prover-service`.
- Prevent duplicate workers from advancing the same proof state concurrently.
- Reject stale updates from workers whose lease expired and was taken over by another worker.

## New Types

```rust
enum ProofStatus {
    /// Waiting for a worker to pick up the initial request.
    Queued,
    /// A worker claimed the request and is starting backend work.
    ///
    /// For Nitro this may directly produce the final proof.
    /// For SP1 this should be short-lived: build witness, submit range proof
    /// request to SP1 network, persist backend id.
    Starting,
    /// External backend work has been requested and is being polled.
    ///
    /// For SP1 this covers range proof and aggregation proof phases.
    BackendPending,
    /// Final proof generated and available via `get_proof`.
    Completed,
    /// Permanently failed after exhausting retries or receiving terminal backend failure.
    Failed,
}

enum BackendProofJobStatus {
    /// External backend work was requested and is awaiting polling.
    Requested,
    /// External backend work completed successfully.
    Completed,
    /// External backend work reached a terminal failure.
    Failed,
}

struct BackendProofId(B256);

enum BackendProofPhase {
    /// SP1 compressed range proof phase.
    Range,
    /// SP1 aggregation proof phase.
    Aggregation,
    /// One external backend request directly produces the final proof.
    Single,
}

impl BackendProofPhase {
    /// Stable database representation stored in `proof_backend_jobs.phase`.
    fn as_str(self) -> &'static str {
        match self {
            Self::Range => "range",
            Self::Aggregation => "aggregation",
            Self::Single => "single",
        }
    }
}

/// Opaque token returned by the `prover-service` when a worker leases a DB row.
///
/// Every update for that leased row must include the same token. This prevents an old worker from
/// overwriting state after its lease expired and another worker reclaimed the row.
struct LeaseToken(uuid::Uuid);

struct LeasedProofRequest {
    request: ProofRequest,
    lease_token: LeaseToken,
}

struct BackendProofWork {
    proof_request: ProofRequest,
    state: BackendProofState,
}

struct LeasedBackendProofWork {
    backend_job_id: i64,
    work: BackendProofWork,
    lease_token: LeaseToken,
}

enum ProofSubmissionLease {
    /// Final proof was produced while starting the user-facing proof job.
    ///
    /// This is the current Nitro path.
    ProofJob { lease_token: LeaseToken },
    /// Final proof was produced while advancing a durable backend job.
    ///
    /// This is the SP1 aggregation path.
    BackendJob {
        backend_job_id: i64,
        lease_token: LeaseToken,
    },
}

enum BackendProofState {
    /// SP1 compressed range proof.
    Range { id: BackendProofId },
    /// SP1 aggregation proof.
    Aggregation { id: BackendProofId },
    /// One backend request can directly produce the final proof.
    ///
    /// Not used by Nitro today because Nitro completes synchronously, but useful if Nitro becomes
    /// async later.
    Single { id: BackendProofId },
}

impl BackendProofState {
    fn phase(self) -> BackendProofPhase;
    fn id(self) -> BackendProofId;
    fn from_phase(phase: BackendProofPhase, id: BackendProofId) -> Self;
}

enum BackendUpdate {
    /// Backend work has been requested externally and must be persisted for later polling.
    Pending {
        state: BackendProofState,
    },
    /// Final proof is complete and can be stored in `proof_jobs.proof_data`.
    ///
    /// Intermediate SP1 range proofs must not be returned as `Complete`. The SP1 backend consumes
    /// completed range proofs internally and returns
    /// `Pending { state: BackendProofState::Aggregation { .. } }` instead.
    Complete(ProofData),
    /// The backend reached a terminal failure for this proof.
    Failed(String),
    /// The external backend is still working, or a transient polling attempt did not advance state.
    Noop,
}
```

## APIs

The examples omit `Result` to emphasize the state transitions. Real traits should return `Result`
with typed errors.

### Defender API - `ProofRequester`

The defender-facing API stays unchanged:

- `request_proof(ProofRequest) -> ProofRequestId`
- `proof_status(ProofRequestId) -> ProofStatus`
- `get_proof(ProofRequestId) -> ProofResponse`

`proof_status` returns the high-level `proof_jobs.status`. It should not expose backend polling
details by default. If operators need more detail later, add a separate detail API instead of
overloading the simple status enum.

### Worker API - `ProofJobQueue`

- `get_next_proof(ProofBackend) -> Option<LeasedProofRequest>`
- `submit_backend_proof_state(proof_id, backend_proof_state, lease_token)`
- `get_next_backend_proof(ProofBackend) -> Option<LeasedBackendProofWork>`
- `complete_backend_proof_job(backend_job_id, lease_token, next_update)`
- `fail_backend_proof_job(backend_job_id, reason, lease_token)`
- `submit_proof(ProofResponse, ProofSubmissionLease)`
- `fail_proof(proof_id, reason, lease_token)`

`submit_backend_proof_state` is used after `backend.start` successfully launches external backend
work. For SP1 this stores `BackendProofState::Range { id }`, where `id` is the range proof id
returned by the SP1 network, and transitions the user-facing proof job to `BackendPending`.

`complete_backend_proof_job` is a generic name for applying a `BackendUpdate` produced by
`backend.advance`. It can:

- clear the backend job lease and schedule another poll for `Noop`;
- mark the current backend job complete and insert the next backend job for `Pending`;
- mark the backend job and proof job failed for `Failed`;
- mark the backend job complete and store the final proof for `Complete`.

`fail_backend_proof_job` reports that a worker failed to advance a durable backend job during this
attempt. It mirrors `fail_proof` for the start phase: the service clears the lease, schedules the
backend job for another poll, and only marks the backend job plus parent proof job `Failed` once
`advance_attempts` reaches the configured maximum. This is for worker or transient backend errors;
`BackendUpdate::Failed` remains a terminal backend result.

`submit_proof` stores the final proof and marks the user-facing proof job `Completed`. It accepts a
`ProofSubmissionLease` because final proofs can come from two places:

- Nitro currently completes directly from `backend.start`, so submission must validate the
  `proof_jobs.lease_token`.
- SP1 completes from `backend.advance` on the aggregation backend job, so submission must validate
  the `proof_backend_jobs.lease_token` for the aggregation row and mark that row `Completed`. The
  worker reports that path through `complete_backend_proof_job(..., BackendUpdate::Complete(_))`.

The final method names can be adjusted, but every worker update should carry the lease token for
the row being updated.

### Worker Backend API - `ProofJobBackend`

- `lane() -> ProofBackend`
- `start(request: &ProofRequest) -> BackendUpdate`
- `advance(request: &ProofRequest, state: BackendProofState) -> BackendUpdate`

`start` is called once a user-facing proof request is leased from `proof_jobs`.

`advance` is called for already-created external backend work stored in `proof_backend_jobs`.

The generic worker does not understand SP1 range or aggregation internals. It only applies
`BackendUpdate` values. The concrete backend decides whether a completed external backend request is
an intermediate proof or a final proof.

## New Workflows

### SP1

1. `ProofWorker` calls `get_next_proof(Sp1)` to get the next SP1 proof request to satisfy.
2. `prover-service` claims one queued SP1 `proof_jobs` row, sets it to `Starting`, writes
   `locked_until`, writes a fresh `lease_token`, increments `start_attempts`, and returns
   `LeasedProofRequest`.
3. Worker calls `sp1_backend.start(request)`.
4. SP1 backend builds the witness for the entire block range.
5. SP1 backend calls SP1 network `.request()` for the compressed range proof.
6. SP1 backend returns:

   ```rust
   BackendUpdate::Pending {
       state: BackendProofState::Range {
           id: range_backend_id, // SP1 proof request id
       },
   }
   ```

7. Worker calls `submit_backend_proof_state(proof_id, Range { id }, lease_token)`.
8. `prover-service` accepts the update only if the `proof_jobs.lease_token` still matches.
9. `prover-service` inserts a `proof_backend_jobs` row for the SP1 range proof and transitions the
   proof job to:

   ```text
   status = BackendPending
   locked_until = null
   lease_token = null
   ```

10. Later, any SP1 `ProofWorker` calls `get_next_backend_proof(Sp1)`.
11. `prover-service` claims a due `proof_backend_jobs` row, writes `locked_until`, writes a fresh
    `lease_token`, and returns the original request plus `BackendProofState::Range { id }`.
12. Worker calls `sp1_backend.advance(request, BackendProofState::Range { id })`.
13. SP1 backend polls SP1 network for the range proof status.
14. If the range proof is still pending, SP1 backend returns:

    ```rust
    BackendUpdate::Noop
    ```

15. Worker reports the `Noop` update. `prover-service` clears the backend job lease and sets
    `next_poll_at` to a future timestamp.
16. If the range proof is complete, SP1 backend fetches the range proof, builds the aggregation
    proof request, and calls SP1 network `.request()` for the aggregation proof.
17. SP1 backend returns:

    ```rust
    BackendUpdate::Pending {
        state: BackendProofState::Aggregation {
            id: agg_backend_id, // SP1 proof request id
        },
    }
    ```

18. Worker reports the `Pending` update for the leased range backend job.
19. `prover-service` marks the range backend job `Completed` and inserts a new
    `proof_backend_jobs` row for the aggregation proof.
20. Later, any SP1 worker calls `get_next_backend_proof(Sp1)` again.
21. `prover-service` returns the original request plus `BackendProofState::Aggregation { id }`.
22. SP1 backend polls SP1 network for the aggregation proof status.
23. If the aggregation proof is still pending, SP1 backend returns `BackendUpdate::Noop`.
24. If the aggregation proof is complete, SP1 backend validates it against the original
    `ProofRequest`.
25. If valid, SP1 backend returns:

    ```rust
    BackendUpdate::Complete(proof_data)
    ```

26. Worker reports the final update for the leased aggregation backend job:

    ```rust
    complete_backend_proof_job(
        backend_job_id,
        lease_token,
        BackendUpdate::Complete(proof_data),
    )
    ```

    Here `backend_job_id` and `lease_token` are the values returned by
    `get_next_backend_proof(Sp1)` for the aggregation backend job.

27. `prover-service` accepts the submission only if the aggregation
    `proof_backend_jobs.lease_token` still matches.
28. `prover-service` marks the aggregation backend job `Completed`, clears its lease, stores the
    final proof in `proof_jobs.proof_data`, and marks the proof job `Completed`.

### Nitro TEE

1. `ProofWorker` calls `get_next_proof(Nitro)` to get the next Nitro proof request to satisfy.
2. `prover-service` claims one queued Nitro `proof_jobs` row, sets it to `Starting`, writes
   `locked_until`, writes a fresh `lease_token`, increments `start_attempts`, and returns
   `LeasedProofRequest`.
3. Worker calls `nitro_backend.start(request)`.
4. Nitro backend builds the witness for the entire block range and immediately produces the proof.
5. Nitro backend returns:

   ```rust
   BackendUpdate::Complete(proof_data)
   ```

6. Worker crafts `ProofResponse` and calls:

   ```rust
   submit_proof(
       proof_response,
       ProofSubmissionLease::ProofJob { lease_token },
   )
   ```

7. `prover-service` accepts the update only if the `proof_jobs.lease_token` still matches, stores
   the proof, and marks the proof job `Completed`.

Nitro does not need `proof_backend_jobs` today. If Nitro later becomes async, `nitro_backend.start`
can return `Pending { state: BackendProofState::Single { id } }`, and the same backend job polling
machinery can resume it.

## Postgres Tables

### `proof_jobs`

One row per user-facing proof request.

```sql
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
    finished_at         timestamptz null
);
```

Recommended constraints:

```sql
alter table proof_jobs
    add constraint proof_jobs_status_check
    check (status in ('queued', 'starting', 'backend_pending', 'completed', 'failed'));

create index proof_jobs_queued_idx
    on proof_jobs (backend, created_at)
    where status = 'queued';

create index proof_jobs_starting_lease_idx
    on proof_jobs (locked_until)
    where status = 'starting';
```

### `proof_backend_jobs`

Durable external backend work.

The Rust API passes backend progress around as `BackendProofState`. The database stores that enum as
two columns: `phase`, serialized from `BackendProofPhase`, and `backend_proof_id` for the contained
id. Keeping `phase` in SQL makes the state queryable and lets Postgres enforce uniqueness like
`unique (proof_id, phase)`.

```sql
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
    unique (proof_id, phase)
);
```

Recommended constraints:

```sql
alter table proof_backend_jobs
    add constraint proof_backend_jobs_status_check
    check (status in ('requested', 'completed', 'failed'));

alter table proof_backend_jobs
    add constraint proof_backend_jobs_phase_check
    check (phase in ('single', 'range', 'aggregation'));

create index proof_backend_jobs_due_idx
    on proof_backend_jobs (backend, next_poll_at)
    where status = 'requested';

create index proof_backend_jobs_lease_idx
    on proof_backend_jobs (locked_until)
    where status = 'requested';
```

For future SP1 split ranges, add:

```sql
phase_index integer not null default 0,
unique (proof_id, phase, phase_index)
```

and remove or replace `unique (proof_id, phase)`.

## How `locked_until` Works

`locked_until` is a durable lease timestamp. A row is available to claim when:

```sql
locked_until is null or locked_until < now()
```

It prevents two workers from owning the same state transition after the database transaction that
claimed the row has committed.

### `proof_jobs.locked_until`

`proof_jobs.locked_until` protects only the short initial start phase.

It is used when a worker claims a queued proof via `get_next_proof`. The worker owns the row while
it calls `backend.start(request)`.

Claiming a proof job should happen inside a transaction. The claim path must also handle expired
`Starting` jobs. This can be done either by a background reaper that moves expired `Starting` jobs
back to `Queued`, or directly in the claim query. The direct approach looks like this:

```sql
select proof_id
from proof_jobs
where backend = $1
  and (
    status = 'queued'
    or (status = 'starting' and locked_until < now())
  )
order by created_at
for update skip locked
limit 1;
```

Then:

```sql
update proof_jobs
set status = 'starting',
    locked_until = now() + $start_lease_timeout,
    lease_token = $lease_token,
    start_attempts = start_attempts + 1,
    updated_at = now()
where proof_id = $proof_id;
```

If the selected row was an expired `Starting` row and `start_attempts` has already reached the
configured maximum, the service should mark it `Failed` instead of leasing it again.

For SP1, this lease should cover witness generation and the SP1 network `.request()` call. It should
not cover the time spent waiting for SP1 network proof generation.

Once SP1 `start()` successfully returns a range backend id, the proof job moves to:

```text
status = BackendPending
locked_until = null
lease_token = null
```

At that point, the long-running proof generation is represented by `proof_backend_jobs`, and the
original proof job must not be leased by `get_next_proof` again.

If the worker dies during `start()`, `locked_until` eventually expires. The job can then be retried
by another worker.

### `proof_backend_jobs.locked_until`

`proof_backend_jobs.locked_until` protects backend polling and phase advancement.

It is used when a worker claims backend work via `get_next_backend_proof`. This prevents duplicate
workers from polling the same completed SP1 range proof and both requesting aggregation proofs.

Claiming backend work should happen inside a transaction:

```sql
select id
from proof_backend_jobs
where backend = $1
  and status = 'requested'
  and next_poll_at <= now()
  and (locked_until is null or locked_until < now())
order by next_poll_at
for update skip locked
limit 1;
```

Then:

```sql
update proof_backend_jobs
set locked_until = now() + $backend_job_lease_timeout,
    lease_token = $lease_token,
    updated_at = now()
where id = $backend_job_id;
```

If `advance()` returns `Noop`, clear the lease and schedule the next poll:

```sql
update proof_backend_jobs
set locked_until = null,
    lease_token = null,
    next_poll_at = now() + $poll_interval,
    updated_at = now()
where id = $backend_job_id
  and lease_token = $lease_token;
```

If the worker dies while advancing backend work, it never clears the lease. After
`locked_until < now()`, another worker can claim and retry that backend job.

## Why Lease Tokens Are Needed

`locked_until` alone prevents simultaneous claims, but it does not protect against stale updates
from slow workers.

Example:

1. Worker A claims a queued SP1 proof:

   ```text
   status = Starting
   locked_until = now() + 10 minutes
   lease_token = A
   ```

2. Worker A is slow and the lease expires.
3. Worker B claims the same proof:

   ```text
   status = Starting
   locked_until = now() + 10 minutes
   lease_token = B
   ```

4. Worker A returns a failure.

The service must ignore Worker A's update because it carries stale token `A`.

Every update should therefore include the lease token, and the SQL update should check it:

```sql
update proof_jobs
set status = 'failed',
    failure_reason = $reason,
    locked_until = null,
    lease_token = null,
    finished_at = now(),
    updated_at = now()
where proof_id = $proof_id
  and status = 'starting'
  and lease_token = $lease_token;
```

If this update affects zero rows, the lease is stale. The service should return a stale lease error
or treat it as a no-op. It must not overwrite the newer owner.

The same rule applies to `proof_backend_jobs`. A stale worker must not be able to:

- mark a backend job failed after another worker reclaimed it;
- request and persist a duplicate next phase;
- submit a final proof after the job already transitioned elsewhere.

## Attempts

The two attempts counters measure different retry layers.

### `proof_jobs.start_attempts`

`start_attempts` counts how many times workers tried to start backend work for the user-facing proof
request.

It increments when `get_next_proof` successfully leases a queued proof job and transitions it to
`Starting`.

It covers failures before durable external backend work has been persisted:

- witness generation fails;
- SP1 network `.request()` fails before returning an SP1 proof id;
- Nitro enclave proving fails;
- worker dies while the proof job is `Starting`;
- the proof job start lease expires before the worker reports an update.

For SP1, once `start()` succeeds and the range `backend_proof_id` is persisted,
`start_attempts` should stop changing. Further progress is tracked by `proof_backend_jobs`.

If `start_attempts` reaches the configured maximum, the service should mark the proof job `Failed`
instead of requeueing it.

### `proof_backend_jobs.advance_attempts`

`advance_attempts` counts failed attempts to advance an already-created external backend job. It
must not count healthy polls where the backend is still working.

It increments when a worker reports an advance error through `fail_backend_proof_job`, or when
`get_next_backend_proof` reclaims an expired backend lease from a worker that died or stopped
responding. A normal lease does not increment it, and `BackendUpdate::Noop` does not increment it.

It covers failures after the external backend id is known:

- transient SP1 status polling errors, but not ordinary "still pending" statuses;
- worker dies while polling or fetching the completed SP1 proof;
- range proof completes, but requesting the aggregation proof fails;
- transient serialization or validation errors while building the next phase;
- stale backend job lease expiry.

An SP1 range backend job and an SP1 aggregation backend job each have their own
`advance_attempts`.

If `advance_attempts` reaches the configured maximum for a backend job, the service should mark the
backend job `Failed` and also mark the parent proof job `Failed`, unless the failure is classified
as indefinitely retryable.

When `advance()` returns a worker error, the worker reports it through `fail_backend_proof_job`, so
transient polling or network errors retry according to `advance_attempts`. When `advance()` returns
`BackendUpdate::Failed`, the service treats it as terminal backend failure and fails the backend job
plus parent proof job immediately.

## Why `next_poll_at` Exists

Workers can wake up periodically, but the database should decide which backend jobs are due.

`next_poll_at` allows per-job scheduling and backoff:

```sql
where status = 'requested'
  and next_poll_at <= now()
```

Without `next_poll_at`, every worker wake-up would scan or poll every pending SP1 backend job. That
would be noisy, expensive, and would make transient-error backoff harder.

With `next_poll_at`:

- normal pending SP1 work can be polled every fixed interval;
- transient errors can use exponential backoff;
- different phases can use different polling intervals;
- workers can run frequently without hammering the SP1 network.

## State Transition Summary

### SP1 success path

```text
proof_jobs:
Queued
  -> Starting
  -> BackendPending
  -> Completed

proof_backend_jobs:
Range Requested
  -> Range Completed
  -> Aggregation Requested
  -> Aggregation Completed
```

### Nitro success path

```text
proof_jobs:
Queued
  -> Starting
  -> Completed

proof_backend_jobs:
no rows
```

### SP1 range still pending

```text
proof_jobs:
BackendPending

proof_backend_jobs:
Range Requested
  locked_until = null
  next_poll_at = future timestamp
```

### Worker dies during SP1 `start`

```text
proof_jobs:
Starting with expired locked_until
  -> claimed again as Starting by another worker, or requeued by a background reaper

proof_backend_jobs:
no row unless the first worker successfully persisted a backend id
```

If SP1 accepted the request but the worker died before persisting the SP1 proof id, that external
SP1 request may be orphaned. Avoiding this requires idempotent SP1 request creation or a separate
pre-submission record. For a first version, this is an acceptable tradeoff to document.

### Worker dies while polling SP1 backend work

```text
proof_jobs:
BackendPending

proof_backend_jobs:
Requested with expired locked_until
  -> claimed by another worker
```

The next worker polls the same persisted SP1 proof id and continues from the durable backend state.
