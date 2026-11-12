# Local proxyd preconfirmed-log qualification

[`proxyd_preconfirmed_logs_test.go`](proxyd_preconfirmed_logs_test.go) is a focused
compatibility fixture for upstream `ethereum-optimism/infra` tag
`proxyd/v4.27.1`, commit `89ca421d0f7c8fe7c16b0a2d08673b43749071c1`
(annotated tag object `68fedbeb681461a1d5619a2455063776d594fa37`).
It reuses that version's existing integration-test helpers and dependencies;
it does not add Go dependencies to the World Chain workspace.

The local run passed with Go `1.27.1` on macOS arm64 on 2026-11-11:

```text
ok github.com/ethereum-optimism/infra/proxyd/integration_tests
```

The fixture starts proxyd's real `HandleWS` handler with its existing auth and
whitelist configuration, an ephemeral-port frontend, and a WebSocket mock
backend. It verifies:

- Byte-for-byte forwarding of `op_supportedCapabilities`,
  `worldchain_getPreconfirmedLogs`, `worldchain_subscribe`, and
  `worldchain_unsubscribe` requests and responses.
- Unchanged `worldchain_subscription` notifications containing nested update
  results, an opaque cursor, and terminal errors, including string and
  maximum-uint64 subscription IDs; unsubscribe `true` is preserved.
- Forwarded WebSocket requests stay on one backend in this pinned version.
- Missing and invalid test auth paths return HTTP 401 before backend connection;
  valid test auth permits the handshake.
- A custom method omitted from the whitelist returns `-32601` without reaching
  the backend.

To reproduce from an existing clean upstream checkout, create a detached
worktree at the pinned commit and copy the fixture into its existing test suite:

```sh
git -C /path/to/optimism-infra worktree add --detach /tmp/proxyd-preconfirmed-check \
  89ca421d0f7c8fe7c16b0a2d08673b43749071c1
cp docs/support/proxyd_preconfirmed_logs_test.go \
  /tmp/proxyd-preconfirmed-check/proxyd/integration_tests/worldchain_preconfirmed_logs_test.go
cd /tmp/proxyd-preconfirmed-check/proxyd
go test -mod=readonly -run '^TestWorldchainPreconfirmedLogsWS$' -count=1 -timeout=30s ./integration_tests
```

The copy command runs from the World Chain root. The fixture uses only local
ephemeral ports, two-second client deadlines, and fake test auth. No deployed
proxy, actual node feed, production authentication, backend failover/HTTP
affinity, cache policy, throughput, or slow-consumer behavior is qualified by
this check. Production whitelist/routing changes and a real node behind the
actual proxy remain rollout requirements in [the API guide](../preconfirmed-logs.md).

The connection-affinity result does not cover HTTP-mapped queries in proxyd
v4.34.1: its [WebSocket request handler](https://github.com/ethereum-optimism/infra/blob/a0201bebab87337b44f70f70fcf921bb2330080c/proxyd/server.go#L998)
intercepts mapped methods and forwards them through the HTTP backend pipeline.
Query and subscription cursors therefore need explicit backend affinity or
resynchronization even when the client uses one WebSocket connection.
