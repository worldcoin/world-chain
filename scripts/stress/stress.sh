#!/usr/bin/env bash

set -euo pipefail

if [[ "${TRACE:-0}" == "1" ]]; then
    set -x
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
STRESS_SCENARIO="${SCRIPT_DIR}/scenarios/stress.toml"
PRECOMPILE_STRESS_SCENARIO="${SCRIPT_DIR}/scenarios/precompileStress.toml"

ENDPOINTS_FILE="${WORLD_CHAIN_DEVNET_ENDPOINTS_FILE:-target/devnet/endpoints.json}"
LOCAL_RPC_FALLBACK="${LOCAL_RPC_FALLBACK:-http://localhost:8545}"

resolve_devnet_endpoint() {
    local key="$1"

    if [[ ! -f "$ENDPOINTS_FILE" ]] || ! command -v jq >/dev/null 2>&1; then
        return 1
    fi

    jq -er --arg key "$key" '.[$key] // empty' "$ENDPOINTS_FILE" 2>/dev/null
}

resolve_endpoint() {
    local override="${1:-}"
    local key="$2"
    local fallback="$3"
    local resolved=""

    if [[ -n "$override" ]]; then
        printf '%s\n' "$override"
        return 0
    fi

    resolved="$(resolve_devnet_endpoint "$key" || true)"
    if [[ -n "$resolved" ]]; then
        printf '%s\n' "$resolved"
        return 0
    fi

    printf '%s\n' "$fallback"
}

RPC_URL="$(resolve_endpoint "${RPC_URL:-}" "sequencer_rpc_url" "$LOCAL_RPC_FALLBACK")"
BUILDER="$(resolve_endpoint "${BUILDER:-}" "sequencer_rpc_url" "$RPC_URL")"

PRIVATE_KEY=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
SEED="${SEED:-0x$(openssl rand -hex 32)}"

run_setup() {
    local scenario_file="$1"

    contender setup \
        -p "$PRIVATE_KEY" \
        "$scenario_file" \
        -r "$RPC_URL" \
        --optimism
}

run_spam() {
    local scenario_file="$1"

    contender spam \
        --builder-url "$BUILDER" \
        --txs-per-second "${TPS:-50}" \
        --duration "${DURATION:-600}" \
        --seed "$SEED" \
        -p "$PRIVATE_KEY" \
        "$scenario_file" \
        -r "$RPC_URL" \
        --optimism \
        --min-balance 0.7eth
}

stress() {
    run_setup "$STRESS_SCENARIO"
    run_spam "$STRESS_SCENARIO"
}

stress_precompile() {
    run_setup "$PRECOMPILE_STRESS_SCENARIO"
    run_spam "$PRECOMPILE_STRESS_SCENARIO"
}

generate_report() {
    contender report
}

case "${1:-}" in
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
    echo "Usage: $0 {stress-precompile|stress|report}"
    echo "Commands:"
    echo "  stress-precompile: Run the precompile stress test"
    echo "  stress: Run the normal stress test"
    echo "  report: Generate a report for the previous stress test"
    exit 1
    ;;
esac
