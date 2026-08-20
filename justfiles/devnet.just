# Local devnets: the native Rust HA devnet (xtask) and the Kurtosis devnet.

# Manage the native Rust HA devnet. Use `just devnet up -d` to run in the background and `just devnet down` to stop it.
# Set BAL=1 to enable flashblocks block access lists on the sequencer nodes.
[doc("Manage the native Rust HA devnet: up / down / status. BAL=1 enables block access lists.")]
[group('devnet')]
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
[group('devnet')]
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

# Launch a local playground (in-process node swarm)
[group('devnet')]
playground *args='':
    RUST_LOG="info" cargo run -p xtask --release -- launch-node $@

# Spawn the Kurtosis devnet (builds the container image first).
[group('devnet')]
devnet-up: build
    @just ./pkg/devnet/devnet-up

# Run Contender stress tests against a running native Rust devnet.
[group('devnet')]
stress *args='':
    @scripts/stress/stress.sh $@
