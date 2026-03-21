set positional-arguments := true

# default recipe to display help information
default:
    @just --list

build:
    docker buildx build \
        -t world-chain:latest .

deploy-contracts:
    @just ./contracts/deploy-contracts

test *args='':
    RUST_LOG="info" cargo nextest run --workspace $@

fmt: fmt-fix fmt-check contracts-fmt

contracts-fmt:
    @just ./contracts/fmt

fmt-fix:
    cargo +nightly fmt --all

fmt-check:
    cargo +nightly fmt --all -- --check

# Launch a local playground (in-process node swarm)
playground *args='':
    RUST_LOG="info" cargo run -p xtask --release -- playground $@

# Run stress tests against a live network
stress *args='':
    RUST_LOG="info" cargo run -p xtask --release -- stress $@

# Prove a PBH transaction
prove *args='':
    cargo run -p xtask -- prove $@

# Generate CLI reference docs for the mdbook
docs:
    cargo xtask docs

install *args='':
    cargo install --path crates/bin/world-chain --locked $@
