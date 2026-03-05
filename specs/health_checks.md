# Health Checks

World Chain nodes expose a configurable health check HTTP server for use with Kubernetes liveness, readiness, and startup probes.

## CLI Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--health.addr` | `0.0.0.0:8080` | Address for the health HTTP server |
| `--health.config` | _(none)_ | Path to a JSON config file. If omitted, all probes return 200 OK unconditionally. |

## HTTP Endpoints

| Endpoint | Kubernetes probe |
|----------|-----------------|
| `GET /startup` | startupProbe |
| `GET /ready` | readinessProbe |
| `GET /live` | livenessProbe |

Each endpoint returns HTTP 200 when healthy, 503 when unhealthy, with a JSON body:

```json
{
  "healthy": true,
  "checks": [
    { "name": "heartbeat", "healthy": true },
    { "name": "min_peers", "healthy": false, "detail": "peers: 0/1" }
  ]
}
```

All configured checks run regardless of overall result — no short-circuiting. The `detail` field is only present on failing checks.

## Background Evaluation

Checks run on a background timer, not on each HTTP request. On startup, all probes are evaluated once before the server begins accepting connections. After that, each probe re-evaluates on its configured `interval_secs`. HTTP handlers return the most recently cached result.

This means: a probe that has never been polled by Kubernetes will still detect block stalls, disk exhaustion, etc., and the cached result will already reflect the failure when the first request arrives.

## Configuration File

The config file is JSON. All fields have defaults — an empty object `{}` is valid.

```json
{
  "startup": {
    "interval_secs": 10,
    "checks": [
      { "type": "block_progress", "mode": "last", "period_secs": 300 }
    ]
  },
  "readiness": {
    "interval_secs": 15,
    "checks": [
      { "type": "not_syncing" },
      { "type": "min_peers", "min": 2 }
    ]
  },
  "liveness": {
    "interval_secs": 30,
    "checks": [
      { "type": "heartbeat" },
      { "type": "block_timestamp", "max_age_secs": 120 },
      { "type": "disk_space", "path": "/var/lib/worldchain", "min_gb": 20.0 }
    ]
  }
}
```

### Top-level fields

| Field | Default |
|-------|---------|
| `startup` | empty probe (always healthy) |
| `readiness` | empty probe (always healthy) |
| `liveness` | empty probe (always healthy) |

### Probe fields

| Field | Default | Description |
|-------|---------|-------------|
| `interval_secs` | `30` | How often to re-evaluate checks in the background |
| `checks` | `[]` | List of check configurations |

## Check Types

### `heartbeat`

Always returns healthy. Useful as a no-op placeholder or to verify the server itself is reachable.

```json
{ "type": "heartbeat" }
```

---

### `min_peers`

Fails if the number of connected peers drops below `min`.

```json
{ "type": "min_peers", "min": 1 }
```

| Field | Default | Description |
|-------|---------|-------------|
| `min` | `1` | Minimum required connected peers |

---

### `not_syncing`

Fails if the node reports that it is currently syncing.

```json
{ "type": "not_syncing" }
```

---

### `block_progress`

Fails if the tracked block number has not advanced within `period_secs`. On the first evaluation the baseline is established and the check returns healthy.

```json
{ "type": "block_progress", "period_secs": 60, "mode": "best" }
```

| Field | Default | Description |
|-------|---------|-------------|
| `period_secs` | `60` | Maximum allowed time (seconds) for the block number to remain unchanged |
| `mode` | `"best"` | Which block number to track: `"best"` (canonical head, fully executed) or `"last"` (downloaded but potentially not yet executed, useful during initial block download) |

Use `mode: "last"` for the startup probe during IBD; use `mode: "best"` for liveness and readiness once the node is in sync.

---

### `block_timestamp`

Fails if the latest block's timestamp is older than `max_age_secs` relative to wall-clock time.

```json
{ "type": "block_timestamp", "max_age_secs": 60 }
```

| Field | Default | Description |
|-------|---------|-------------|
| `max_age_secs` | `60` | Maximum allowed block age in seconds |

---

### `disk_space`

Fails if available disk space on the filesystem containing `path` drops below `min_gb` GiB.

Uses a single `statvfs(2)` syscall — equivalent to `df`, not a recursive directory scan.

```json
{ "type": "disk_space", "path": "/var/lib/worldchain", "min_gb": 10.0 }
```

| Field | Default | Description |
|-------|---------|-------------|
| `path` | _(required)_ | Any path on the filesystem to check |
| `min_gb` | `10.0` | Minimum required free space in GiB |

## Kubernetes Example

```yaml
startupProbe:
  httpGet:
    path: /startup
    port: 8080
  failureThreshold: 30
  periodSeconds: 10

readinessProbe:
  httpGet:
    path: /ready
    port: 8080
  failureThreshold: 3
  periodSeconds: 15

livenessProbe:
  httpGet:
    path: /live
    port: 8080
  failureThreshold: 3
  periodSeconds: 30
```

Pair with a config file:

```json
{
  "startup": {
    "interval_secs": 10,
    "checks": [
      { "type": "block_progress", "mode": "last", "period_secs": 300 }
    ]
  },
  "readiness": {
    "interval_secs": 15,
    "checks": [
      { "type": "not_syncing" },
      { "type": "min_peers", "min": 1 }
    ]
  },
  "liveness": {
    "interval_secs": 30,
    "checks": [
      { "type": "block_timestamp", "max_age_secs": 120 },
      { "type": "disk_space", "path": "/var/lib/worldchain", "min_gb": 10.0 }
    ]
  }
}
```
