set positional-arguments := true
set dotenv-load := true

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

# Heavy: needs Docker, the SP1 toolchain, and ~32-128GB RAM for `cpu`. Pass `network`
# (with SP1_PRIVATE_KEY set) to offload proving. Logs default to the defender/proving
# filter; export RUST_LOG to override (e.g. `RUST_LOG=$PROVING_LOG just defender-e2e`).
# Run the defender SP1 Groth16 e2e test with real proving (prover=cpu|network).
defender-e2e prover='cpu' *args='':
    #!/usr/bin/env bash
    set -euo pipefail
    # The devnet spawns the compiled world-chain node as a subprocess, so build it first.
    cargo build -p world-chain
    default_log="warn,world_chain_tests=info,world_chain_defender=trace,world_chain_challenger=trace,world_chain_prover_service=trace,world_chain_sp1_worker=trace,world_chain_proof_worker=debug,world_chain_proof_succinct_host_utils=info,world_chain_devnet=info,op_batcher=error"
    RUST_LOG="${RUST_LOG:-$default_log}" DEVNET_SP1_WORKER_PROVER="{{prover}}" \
        cargo test -p world-chain-tests -- --ignored --nocapture \
        defender_finalizes_challenged_game_with_sp1_proof {{args}}

clippy:
    cargo +nightly clippy --workspace --all-targets --all-features

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
# Set BAL=1 to enable flashblocks block access lists on the sequencer nodes.
devnet command='up' *args='':
    #!/usr/bin/env bash
    set -euo pipefail
    EXTRA_ARGS=()
    if [ "{{command}}" = "up" ]; then
        cargo build -p world-chain
        if [ "${BAL:-0}" = "1" ]; then
            EXTRA_ARGS+=(--bal-enabled)
        fi
    fi
    RUST_LOG="${RUST_LOG:-info,flashblocks=trace,engine_driver=info}" cargo run -p xtask -- devnet {{command}} {{args}} ${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}

# Tail world-chain execution client logs from the running devnet (e.g. `just devnet-logs` or `just devnet-logs 0` for a specific sequencer).
devnet-logs index='':
    #!/usr/bin/env bash
    set -uo pipefail
    LOG_FILE="${WORLD_CHAIN_DEVNET_LOG_FILE:-target/devnet/logs/devnet.log}"
    if [ ! -f "$LOG_FILE" ]; then
        echo "no devnet log file at $LOG_FILE; is the devnet running?" >&2
        exit 1
    fi
    if [ -n "{{index}}" ]; then
        PATTERN="world-chain-el-{{index}} "
    else
        PATTERN="world-chain-el-"
    fi
    tail -n 200 -F "$LOG_FILE" | grep --line-buffered -- "$PATTERN"

# Run Contender stress tests against a running native Rust devnet.
stress *args='':
    @scripts/stress/stress.sh $@

# Prove a PBH transaction
prove *args='':
    cargo run -p xtask -- prove $@

# Deterministically build the World range SP1 ELF with cargo-prove/Docker
build-proof-range-elf:
    cd proofs/succinct/programs/range-ethereum && cargo prove build --docker --workspace-directory ../../../.. --tag v6.1.0 --ignore-rust-version --elf-name world-chain-range-ethereum --output-directory ../../elf

# Deterministically build the World aggregation SP1 ELF with cargo-prove/Docker
build-proof-aggregation-elf:
    cd proofs/succinct/programs/aggregation && cargo prove build --docker --workspace-directory ../../../.. --tag v6.1.0 --ignore-rust-version --elf-name world-chain-aggregation --output-directory ../../elf

# Build all World SP1 proof ELFs
build-proof-elfs: build-proof-range-elf build-proof-aggregation-elf

# Compute the on-chain verification keys for the committed SP1 proof ELFs
proof-vkeys *args='':
    cargo run --release -p proof --features sp1 -- sp1 vkeys $@

# Generate CLI reference docs for the mdbook
docs:
    cargo xtask docs

install *args='':
    cargo install --path bin/world-chain --locked $@
