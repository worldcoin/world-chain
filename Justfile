set positional-arguments := true

# default recipe to display help information
default:
    @just --list

# Spawns the devnet
devnet-up: deploy-devnet deploy-contracts

deploy-devnet: build
    just ./devnet/devnet-up

build:
    docker buildx build \
        -t world-chain:latest .

deploy-contracts:
    @just ./contracts/deploy-contracts

# Stops the devnet **This will prune all docker containers**
devnet-down:
    @just ./devnet/devnet-down

test *args='':
    RUST_LOG="info" cargo nextest run --workspace $@

# Formats the whole workspace
fmt: devnet-fmt contracts-fmt fmt-fix fmt-check

devnet-fmt:
    @just ./devnet/fmt

contracts-fmt:
    @just ./contracts/fmt

fmt-fix:
    cargo +nightly fmt --all

fmt-check:
    cargo +nightly fmt --all -- --check

e2e-test *args='':
    RUST_LOG="info,tests=info" cargo run -p tests-devnet --release -- $@

install *args='':
    cargo install --path crates/world/bin --locked $@

stress-test *args='':
    @just ./devnet/stress-test $@
