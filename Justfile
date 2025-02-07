set positional-arguments

# default recipe to display help information
default:
    @just --list

# Spawns the devnet
devnet-up: deploy-devnet deploy-contracts

deploy-devnet:
    @just ./devnet/devnet-up

deploy-contracts:
    @just ./contracts/deploy-contracts

# Stops the devnet **This will prune all docker containers**
devnet-down:
    @just ./devnet/devnet-down

e2e-test *args='':
    @just ./world-chain-builder/e2e-test $@

# Builds and tags the world-chain-builder image
build:
    @just ./devnet/build

# Tests the world-chain-builder
test:
    @just ./world-chain-builder/test

# Formats the world-chain-builder
fmt: 
    @just ./world-chain-builder/fmt

# Installs the world-chain-builder
install *args='':
    @just ./world-chain-builder/install $@
