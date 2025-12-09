# World Chain Devnet

The World Chain Devnet is an wrapper around the [optimism-package](https://github.com/ethpandaops/optimism-package/) used to test the World Chain Block builder.

# Deployment

To deploy the devnet first make sure you have [kurtosis-cli](https://docs.kurtosis.com/install), and [just](https://github.com/casey/just?tab=readme-ov-file#installation) installed.

## Build or Pull `world-chain` image

It's likely you want to build & test a devnet against your working copy:

```bash
# build & tag image from repo root
cd ../
just build
```

If you want to use any pre-existing or alternative image, modify the [network_params.yaml](network_params.yaml) file, or pull and tag one:

```bash
# pull from github & tag
docker pull ghcr.io/worldcoin/world-chain:latest
docker tag ghcr.io/worldcoin/world-chain:latest world-chain:latest
```

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
