# World Chain Devnet

The World Chain Devnet is an wrapper around the [optimism-package]() used to test the World Chain Block builder.

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
just stress-test <stress | stress-precompile>

# Generate a performance report
just stress-test report
```

# Grafana

The devnet observability stack includes Grafana and Prometheus. This repo currently does not
provision a custom World Chain dashboard into that Grafana automatically, so import the dashboard
JSON manually from:

```text
devnet/grafana/dashboards/flashblocks-payload-builder.json
```

It is built for the `reth_flashblocks_payload_build_*` metrics exposed by the flashblocks-enabled
payload builder.
