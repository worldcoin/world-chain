set positional-arguments := true

# default recipe to display help information
default:
    @just --list

build:
    docker buildx build \
        -t world-chain:latest .

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

# === Profiling ===

# Build with profiling profile (release + debug symbols for perf/samply/flamegraph)
build-profiling features='jemalloc':
    RUSTFLAGS="-C target-cpu=native" cargo build --profile profiling --features {{features}} --bin world-chain

# Build with maxperf profile + native CPU targeting
build-maxperf features='jemalloc':
    RUSTFLAGS="-C target-cpu=native" cargo build --profile maxperf --features {{features}} --bin world-chain

# Build release targeting native CPU (without profiling symbols)
build-native features='jemalloc':
    RUSTFLAGS="-C target-cpu=native" cargo build --release --features {{features}} --bin world-chain

# Run benchmarks with profiling profile
bench-profiling *args='':
    RUSTFLAGS="-C target-cpu=native" cargo bench --profile profiling --package world-chain-builder --bench coordinator -- $@

# Run playground with profiling build for profile data collection
stress-profile *args='':
    RUSTFLAGS="-C target-cpu=native" RUST_LOG="info" cargo run -p xtask --profile profiling -- launch-node --nodes 2 --spam --num-blocks 100 $@

# Build Docker image with profiling profile
docker-profiling features='jemalloc':
    docker buildx build \
        --build-arg PROFILE=profiling \
        --build-arg FEATURES={{features}} \
        -t world-chain:profiling .

# Build Docker image with maxperf profile
docker-maxperf features='jemalloc':
    docker buildx build \
        --build-arg PROFILE=maxperf \
        --build-arg FEATURES={{features}} \
        -t world-chain:maxperf .

# === PGO (Profile-Guided Optimization) ===

# PGO phase 1: build with instrumentation
build-pgo-instrumented pgo_data_dir='/tmp/pgo-data' features='jemalloc':
    mkdir -p {{pgo_data_dir}}
    RUSTFLAGS="-Cprofile-generate={{pgo_data_dir}} -C target-cpu=native" \
        cargo build --profile profiling --features {{features}} --bin world-chain

# Merge PGO profile data files into a single .profdata
merge-pgo pgo_data_dir='/tmp/pgo-data':
    $(rustc --print sysroot)/lib/rustlib/$(rustc -vV | awk '/^host:/ { print $$2 }')/bin/llvm-profdata \
        merge -o {{pgo_data_dir}}/merged.profdata {{pgo_data_dir}}

# PGO phase 3: build with optimization using collected profile data
build-pgo-optimized pgo_merged='/tmp/pgo-data/merged.profdata' features='jemalloc':
    RUSTFLAGS="-Cprofile-use={{pgo_merged}} -C target-cpu=native" \
        cargo build --profile maxperf --features {{features}} --bin world-chain
