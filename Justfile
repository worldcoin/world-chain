set positional-arguments

# default recipe to display help information
default:
    @just --list

# Spawns the devnet
devnet-up:
    @just ./devnet/devnet-up

# Stops the devnet **This will prune all docker containers**
devnet-down:
    @just ./devnet/devnet-down

e2e-test:
    @just ./world-chain-builder/e2e-test

# Builds and tags the world-chain-builder image
build:
    @just ./devnet/build

# Tests the world-chain-builder
test:
    @just ./world-chain-builder/test

# Tests the world-chain-builder and generates a report
test-ci:
    @just ./world-chain-builder/test-ci

# Formats the world-chain-builder
fmt: 
    @just ./world-chain-builder/fmt
