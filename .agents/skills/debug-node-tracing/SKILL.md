---
name: debug-node-tracing
description: Debug World Chain nodes with temporary admin_tracingDirectives log filters and targeted log collection. Use for live Flashblocks, networking, pool, builder, or validator debugging; not EVM transaction traces.
---

# Debug node tracing

Use World Chain's `admin_tracingDirectives` RPC to change a running node's log filter for a bounded time. Adapted from the local `nodl` skill; verify behavior against `crates/rpc/src/admin.rs`, `crates/cli/src/app.rs`, and `crates/node/src/add_ons.rs`.

## Check the target

- Establish the environment, pod, symptom, and time window from the debugging request. Start with one pod and a narrow target at `debug` for 120 seconds; use `trace` only when needed.
- Check the deployed image and startup flags. Reload requires `admin` in an enabled `--http.api` or `--ws.api` at startup. This RPC uses that HTTP/WS server, not the Engine API on 8551.
- Inspect startup stdout and file filters before changing them. Reload affects all reloadable outputs; expiry restores the stdout startup filter to every output. A different file filter is lost until restart. Preserve existing diagnostic coverage when choosing whether to use this RPC.
- Overrides are process-wide. A new call replaces the active override and its timer. Do not interrupt another debugging session; renew deliberately before expiry when more time is needed.

## Connect and apply

Builders require VPN access and pod port-forwarding in `world-chain-builder`. Contexts are `tfh-crypto-{dev,stage,prod}-eu-central-2`. Load the matching JWT from the configured secret store without printing it; port 8545 also requires authentication in this deployment.

Stage example; change the context, pod, and credential together for another target:

```bash
kubectl --context tfh-crypto-stage-eu-central-2 -n world-chain-builder \
  port-forward --address 127.0.0.1 --pod-running-timeout=30s \
  pod/world-chain-builder-0 18545:8545
```

Keep that process running while collecting logs. Select targets from the deployed code's `target:` fields; examples in this repository include `flashblocks::p2p`, `flashblocks::payload_builder`, and `flashblocks::state_executor`.

The following example assumes the stdout baseline is `info`. Preserve any required baseline directives in the replacement filter:

```bash
ETH_RPC_JWT_SECRET="$JWT_STAGE" cast rpc admin_tracingDirectives \
  '{"directives":"info,flashblocks::p2p=debug","ttlSecs":120}' \
  --rpc-url http://localhost:18545 --rpc-timeout 15
```

`cast` mints the bearer token from the raw JWT secret. Its flag is `--jwt-secret`; `nodl` uses `--jwt` and does not wrap this custom method. Never print credentials or enable shell tracing around authenticated commands.

Use camelCase `ttlSecs`. The allowed TTL is 1–3600 seconds; trimmed directives must be nonempty and at most 1024 bytes. Save the successful response's `applied`, `ttlSecs`, and `revertsTo` fields. A timeout leaves application uncertain; inspect logs before retrying, since a retry replaces the timer.

## Collect and restore

1. Filter Datadog logs by environment, pod, target, and the capture window. If DEBUG/TRACE is not shipped, inspect bounded pod logs or the configured on-pod log file.
2. Correlate block, payload, transaction, and peer identifiers relevant to the symptom. Avoid collecting unrelated payloads or logging secrets.
3. Let the TTL expire, or restore early by calling the same method with the returned `revertsTo` as `directives` and `ttlSecs: 1`. Do this only while your override still owns the session; do not overwrite a newer operator's filter.
4. Check for the `world_chain::admin` revert message or a revert failure. If restoration cannot be observed, report it as unverified. Stop the port-forward you started.
5. Report the target/image, filter, capture window, evidence, and restoration status. Do not restart a node or change its deployment unless the task authorizes it.

## Failures

- `401`: verify the environment's JWT and forwarded port; do not remove authentication.
- `-32601 Method not found`: check the deployed version, namespace, and port. Local source alone does not establish deployed support.
- `tracing reload is not active`: startup did not install reload support. Report the required startup flag change; it cannot be enabled through this RPC.
