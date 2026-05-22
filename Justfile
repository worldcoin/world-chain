set positional-arguments := true

# default recipe to display help information
default:
    @just --list

build:
    docker buildx build \
        --build-arg VERGEN_GIT_SHA="$(git rev-parse HEAD)" \
        -t world-chain:latest .

build-world-chain-bin:
    cargo build -p world-chain

devnet-up: build
    @just ./pkg/devnet/devnet-up

deploy-contracts:
    @just ./pkg/contracts/deploy-contracts

test *args='':
    RUST_LOG="info" cargo nextest run --workspace $@

# Test with flashblocks debug tracing
test-dev *args='':
    RUST_LOG="info,flashblocks=debug,world_chain=info" cargo nextest run --workspace $@

# Test with verbose flashblocks tracing (all subsystems at trace level)
test-verbose *args='':
    RUST_LOG="info,flashblocks=trace,world_chain=trace,bal_executor=trace,payload_builder=trace,engine::tree=trace" cargo nextest run --workspace $@

fmt: fmt-fix fmt-check contracts-fmt

contracts-fmt:
    @just ./pkg/contracts/fmt

fmt-fix:
    cargo +nightly fmt --all

fmt-check:
    cargo +nightly fmt --all -- --check

# Launch a local playground (in-process node swarm)
playground *args='':
    RUST_LOG="info" cargo run -p xtask --release -- launch-node $@

# Manage the native Rust HA devnet. Use `just devnet up -d` to run in the background and `just devnet down` to stop it.
devnet command='up' *args='':
    #!/usr/bin/env bash
    set -euo pipefail
    if [ "{{command}}" = "up" ]; then
        cargo build -p world-chain
    fi
    RUST_LOG="${RUST_LOG:-info,flashblocks=trace,engine_driver=info}" cargo run -p xtask -- devnet {{command}} {{args}}

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
    cargo install --path bin/world-chain --locked $@
