# World Chain Devnet

The World Chain Devnet is an wrapper around the [optimism-package]() used to test the World Chain Block builder.

# Deployment
To deploy the devnet first make sure you have [kurtosis-cli](https://docs.kurtosis.com/install) and [just](https://just.systems/man/en/) installed.

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

