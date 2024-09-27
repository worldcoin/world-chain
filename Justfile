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

# Builds and tags the world-chain-builder image
build:
    @just ./devnet/build