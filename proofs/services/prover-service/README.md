# World Chain Prover Service

The prover-service persists proof requests and worker leases in Postgres. Workers claim jobs
over JSON-RPC and heartbeat while proving. The defender requests proofs and submits them on-chain.

The service requires `PROVER_SERVICE_DATABASE_URL` and `L1_RPC_URL`. Set `L1_FALLBACK_RPC_URL`
to use the same shared RPC fallback client as the other proof services. `L1_RPC_TIMEOUT_SECONDS`
sets the per-request timeout (default: 10 seconds). Every
`STATUS_POLLER_INTERVAL_SECS` (default: 30 seconds), it checks active jobs against L1 and cancels
them when the game has sufficient proof support or its resolution outcome is no longer in progress.
Failed L1 reads leave jobs untouched. The same loop fails jobs that exhausted their worker attempts.

Cancellation revokes worker leases while preserving completed proofs and backend session records.
Workers stop local work on their next heartbeat; already submitted remote proving requests are not
cancelled at the backend. A new defender request can resume a cancelled proof using existing backend
sessions without consuming a failure retry, for example after a supported game is challenged or
an L1 reorg.

Deploy the updated defender before enabling cleanup: it understands the new `Cancelled` request
status. Database migrations run automatically when prover-service starts.
