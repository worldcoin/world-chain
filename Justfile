set positional-arguments := true

# default recipe to display help information
default:
    @just --list

# Spawns the devnet
devnet-up build_image="true" dev="false": (deploy-devnet build_image dev) deploy-contracts

deploy-devnet build_image dev:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "{{ build_image }}" == "true" ]]; then
        if [[ "{{ dev }}" == "true" ]]; then
            just build-dev
        else
            just build
        fi
    fi
    just ./devnet/devnet-up

build cache_type="local" cache_ref=".docker-cache" profile="maxperf":
    #!/usr/bin/env bash

    if [[ "{{ cache_type }}" == "local" ]]; then
        mkdir -p {{ cache_ref }}
        docker buildx build \
            --build-arg PROFILE={{ profile }} \
            --build-arg FEATURES=jemalloc,test \
            --cache-from type=local,src={{ cache_ref }} \
            --cache-to type=local,dest={{ cache_ref }},mode=max \
            -t world-chain:latest .
    elif [[ "{{ cache_type }}" == "gha" ]]; then
        docker buildx build \
            --build-arg PROFILE={{ profile }} \
            --build-arg FEATURES=jemalloc \
            --cache-from type=gha \
            --cache-to type=gha,mode=max \
            -t world-chain:latest .
    elif [[ "{{ cache_type }}" == "registry" ]]; then
        docker buildx build \
            --build-arg PROFILE={{ profile }} \
            --build-arg FEATURES=jemalloc \
            --cache-from type=registry,ref={{ cache_ref }} \
            --cache-to type=registry,ref={{ cache_ref }},mode=max \
            -t world-chain:latest .
    else
        echo "Unknown cache type: {{ cache_type }}"
        exit 1
    fi

# Fast dev build using dev-docker profile (much faster than maxperf)
build-dev cache_type="local" cache_ref=".docker-cache":
    just build {{ cache_type }} {{ cache_ref }} dev-docker

deploy-contracts:
    @just ./contracts/deploy-contracts

# Stops the devnet **This will prune all docker containers**
devnet-down:
    @just ./devnet/devnet-down

test LEVEL='info' *args='':
    RUST_LOG=LEVEL cargo nextest run --workspace $@

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
