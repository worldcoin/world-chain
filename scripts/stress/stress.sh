#!/usr/bin/env bash

set -euo pipefail

if [[ "${TRACE:-0}" == "1" ]]; then
    set -x
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../.." && pwd)"
STRESS_SCENARIO="${SCRIPT_DIR}/scenarios/stress.toml"
PRECOMPILE_STRESS_SCENARIO="${SCRIPT_DIR}/scenarios/precompileStress.toml"
DEVNET_ENDPOINTS_FILE="${WORLD_CHAIN_DEVNET_ENDPOINTS_FILE:-${REPO_ROOT}/target/devnet/endpoints.json}"
EXPECTED_CHAIN_ID="${DEVNET_CHAIN_ID:-2151908}"
DEVNET_ENDPOINTS_TIMEOUT="${DEVNET_ENDPOINTS_TIMEOUT:-180}"
DEVNET_RPC_TIMEOUT="${DEVNET_RPC_TIMEOUT:-60}"

PRIVATE_KEY=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80

die() {
    echo "error: $*" >&2
    exit 1
}

require_command() {
    local command="$1"

    command -v "$command" >/dev/null 2>&1 || die "required command not found: $command"
}

resolve_devnet_rpc() {
    local elapsed=0
    local resolved

    if [[ -n "${RPC_URL:-}" ]]; then
        printf '%s\n' "$RPC_URL"
        return 0
    fi

    while ((elapsed <= DEVNET_ENDPOINTS_TIMEOUT)); do
        if [[ -f "$DEVNET_ENDPOINTS_FILE" ]]; then
            resolved="$(jq -er '.primary.sequencer_rpc_url // empty' "$DEVNET_ENDPOINTS_FILE" 2>/dev/null || true)"
            if [[ -n "$resolved" ]]; then
                printf '%s\n' "$resolved"
                return 0
            fi
        fi

        if ((elapsed == 0)); then
            echo "Waiting for native devnet endpoints at ${DEVNET_ENDPOINTS_FILE}..." >&2
        fi

        sleep 1
        elapsed=$((elapsed + 1))
    done

    die "native devnet endpoints were not ready at ${DEVNET_ENDPOINTS_FILE}; start the devnet with 'just devnet' or 'just devnet up -d' and wait until it prints endpoints"
}

validate_rpc() {
    local rpc_url="$1"
    local elapsed=0
    local response
    local chain_hex
    local chain_id
    local last_error=""

    while ((elapsed <= DEVNET_RPC_TIMEOUT)); do
        response="$(curl -fsS --max-time "${RPC_CHECK_TIMEOUT:-2}" \
            -H 'content-type: application/json' \
            --data '{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}' \
            "$rpc_url" 2>&1)" && {
            chain_hex="$(printf '%s\n' "$response" | jq -er '.result' 2>/dev/null)" \
                || die "invalid eth_chainId response from ${rpc_url}: ${response}"
            chain_id="$((chain_hex))"

            [[ "$chain_id" -eq "$EXPECTED_CHAIN_ID" ]] \
                || die "expected chain id ${EXPECTED_CHAIN_ID}, got ${chain_id} from ${rpc_url}"

            return 0
        }

        last_error="$response"
        if ((elapsed == 0)); then
            echo "Waiting for native devnet RPC at ${rpc_url}..." >&2
        fi

        sleep 1
        elapsed=$((elapsed + 1))
    done

    die "could not reach native devnet RPC at ${rpc_url} after ${DEVNET_RPC_TIMEOUT}s; is 'just devnet' running? last error: ${last_error}"
}

DEVNET_RPC=""

prepare_devnet_rpc() {
    if [[ -n "$DEVNET_RPC" ]]; then
        return 0
    fi

    require_command jq
    require_command curl
    DEVNET_RPC="$(resolve_devnet_rpc)"
    validate_rpc "$DEVNET_RPC"
    echo "Using native devnet RPC: ${DEVNET_RPC}" >&2
}

run_setup() {
    local scenario_file="$1"

    require_command contender
    contender setup \
        -p "$PRIVATE_KEY" \
        "$scenario_file" \
        -r "$DEVNET_RPC" \
        --optimism
}

run_spam() {
    local scenario_file="$1"
    local seed="${SEED:-}"

    if [[ -z "$seed" ]]; then
        require_command openssl
        seed="0x$(openssl rand -hex 32)"
    fi

    require_command contender
    contender spam \
        --builder-url "$DEVNET_RPC" \
        --txs-per-second "${TPS:-50}" \
        --duration "${DURATION:-60}" \
        --seed "$seed" \
        -p "$PRIVATE_KEY" \
        "$scenario_file" \
        -r "$DEVNET_RPC" \
        --optimism \
        --min-balance 0.7eth
}

stress() {
    prepare_devnet_rpc
    run_setup "$STRESS_SCENARIO"
    run_spam "$STRESS_SCENARIO"
}

stress_precompile() {
    prepare_devnet_rpc
    run_setup "$PRECOMPILE_STRESS_SCENARIO"
    run_spam "$PRECOMPILE_STRESS_SCENARIO"
}

generate_report() {
    require_command contender
    contender report
}

case "${1:-stress}" in
stress-precompile)
    stress_precompile
    ;;
stress)
    stress
    ;;
report)
    generate_report
    ;;
*)
    echo "Usage: $0 [stress|stress-precompile|report]"
    echo "Commands:"
    echo "  stress: Run the normal stress test against the native Rust devnet"
    echo "  stress-precompile: Run the precompile stress test against the native Rust devnet"
    echo "  report: Generate a report for the previous stress test"
    exit 1
    ;;
esac
