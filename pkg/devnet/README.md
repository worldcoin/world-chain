# World Chain Devnet

The World Chain Devnet is an wrapper around the [optimism-package]() used to test the World Chain Block builder.

# Native Rust Devnet

The experimental Rust devnet lives in `crates/devnet` and can be started with:

```bash
just devnet up
```

This path is intended to replace the Kurtosis-based local e2e/devnet flow after parity is proven. It uses a reusable Rust harness with typed topology presets, lifecycle-owned resources, dynamic ports by default, and `testcontainers` for the L1 dev-chain dependency. Endpoint URLs are printed at startup and the process keeps producing blocks until Ctrl-C.

Useful flags:

```bash
just devnet up --stable-ports
just devnet up --observability
just devnet up --print-topology
just devnet up --latest-hardfork tropo
just devnet up --disable-hardfork jovian
just devnet up -d
just devnet down
```

The native `just devnet up` entrypoint starts the HA sequencing target. The lower-level
`DirectSequencer` and `Minimal` presets remain available through `xtask` for narrower tests. All
native presets sequence directly with flashblocks enabled and intentionally do not wire
rollup-boost or tx-proxy.

The HA sequencing target is available as a typed manifest:

```bash
cargo run -p xtask -- devnet up --preset ha-sequencer --print-topology
```

The native devnet writes the same filtered logs to stdout and
`target/devnet/logs/devnet.log`. Process logs use per-service tracing targets, so filters like
`RUST_LOG=op_conductor=error just devnet up` show only conductor errors while preserving the
`process=op-conductor-N` field.

This follows the same shape as Optimism `op-devstack`: typed component handles, explicit topology composition, lifecycle-owned resources, and fresh systems per run. The target component graph is:

- L1 dev chain.
- OP contract deployer using the same artifact locator from `network_params.yaml`.
- Three World Chain execution/client replicas sequencing directly.
- Three `op-node` instances, one per execution replica.
- Three `op-conductor` instances, one per sequencer, with a local raft bootstrap node.
- One `op-batcher`.
- One `op-proposer`.
- One `op-challenger`.
- Prometheus and Grafana.

World contract deployment is intentionally deferred in this native devnet path. `FeeEscrow`,
`FeeRecipient`, rollup-boost, tx-proxy, and rundler are not part of the new default HA path. PBH is
disabled for native World Chain execution nodes with zero reserved PBH blockspace and an undeployed
sentinel PBH entrypoint.

Current parity gaps:

- The HA preset starts `op-node`, `op-conductor`, `op-batcher`, `op-proposer`, `op-challenger`,
  Prometheus, and Grafana. Full fault-proof game execution still needs generated Cannon prestates
  matching the local OP deployment; the current challenger is configured with a lifecycle-owned
  local prestates directory so the service can run and monitor.
- The backup World Chain execution nodes and `op-node` replicas are started for the HA shape, but
  failover behavior is not yet exercised by an automated test.
- Stable ports are available through `--stable-ports`, but tests should continue to use dynamic
  ports.
- `FeeEscrow` and `FeeRecipient` are not deployed by the native devnet. Add a devnet-specific
  deployment path if a future test needs those contracts without changing production contract
  constructor behavior.
- Rundler and the multi-client EL/CL matrix are deferred until they are needed by specific tests or local workflows.

# Deployment
To deploy the devnet first make sure you have [kurtosis-cli](), and [just]() installed.

Then run the following command from the project root:

```bash
just devnet-up
```

# Testing

```bash
# Run E2E Tests
just e2e-test -n

# Run stress tests with contender (requires contender is installed)
# By default the script resolves the builder and tx-proxy URLs from the
# running `world-chain` Kurtosis enclave. Override with `BUILDER=...`,
# `TX_PROXY=...`, or `KURTOSIS_ENCLAVE=...` when needed.
just stress-test <stress | stress-precompile>

# Generate a performance report
just stress-test report
```

# Grafana

The devnet observability stack includes Grafana and Prometheus. `just devnet up` provisions the
Prometheus datasource and imports the World Chain flashblocks dashboards automatically. The startup
output prints the Grafana URL and all component endpoint URLs, including the primary sequencer RPC
port.

Provisioned dashboards:

```text
pkg/devnet/grafana/dashboards/flashblocks-payload-builder.json
pkg/devnet/grafana/dashboards/flashblocks-validation-pipeline.json
pkg/devnet/grafana/dashboards/flashblocks-p2p.json
```

The dashboards are loaded into the `World Chain Devnet` Grafana folder.

- `flashblocks-payload-builder.json` for `reth_flashblocks_payload_build_*`.
- `flashblocks-validation-pipeline.json` for `reth_flashblocks_validation_*`.
- `flashblocks-p2p.json` for `reth_flashblocks_p2p*` P2P metrics, including peer-scoped `peer_id`
  series.
