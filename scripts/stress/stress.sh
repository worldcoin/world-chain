#!/usr/bin/env bash

set -euo pipefail

if [[ "${TRACE:-0}" == "1" ]]; then
    set -x
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
STRESS_SCENARIO="${SCRIPT_DIR}/scenarios/stress.toml"
PRECOMPILE_STRESS_SCENARIO="${SCRIPT_DIR}/scenarios/precompileStress.toml"

KURTOSIS_ENCLAVE="${KURTOSIS_ENCLAVE:-world-chain}"
KURTOSIS_BUILDER_SERVICE="${KURTOSIS_BUILDER_SERVICE:-op-el-builder-2151908-1-custom-op-node-op-kurtosis}"
KURTOSIS_TX_PROXY_SERVICE="${KURTOSIS_TX_PROXY_SERVICE:-tx-proxy}"

LOCAL_BUILDER_FALLBACK="${LOCAL_BUILDER_FALLBACK:-http://localhost:8545}"
LOCAL_TX_PROXY_FALLBACK="${LOCAL_TX_PROXY_FALLBACK:-http://localhost:8545}"

resolve_kurtosis_url() {
    local service_name="$1"
    local port_name="$2"
    local output
    local url
    local host_port

    if ! command -v kurtosis >/dev/null 2>&1; then
        return 1
    fi

    output="$(kurtosis port print "$KURTOSIS_ENCLAVE" "$service_name" "$port_name" 2>/dev/null || true)"
    url="$(printf '%s\n' "$output" | sed -nE 's/.*(https?:\/\/127\.0\.0\.1:[0-9]+).*/\1/p' | head -n1)"
    if [[ -n "$url" ]]; then
        printf '%s\n' "$url"
        return 0
    fi

    host_port="$(printf '%s\n' "$output" | sed -nE 's/.*(127\.0\.0\.1:[0-9]+).*/\1/p' | head -n1)"
    if [[ -n "$host_port" ]]; then
        printf 'http://%s\n' "$host_port"
        return 0
    fi

    return 1
}

resolve_endpoint() {
    local override="${1:-}"
    local service_name="$2"
    local port_name="$3"
    local fallback="$4"
    local resolved=""

    if [[ -n "$override" ]]; then
        printf '%s\n' "$override"
        return 0
    fi

    resolved="$(resolve_kurtosis_url "$service_name" "$port_name" || true)"
    if [[ -n "$resolved" ]]; then
        printf '%s\n' "$resolved"
        return 0
    fi

    printf '%s\n' "$fallback"
}

BUILDER="$(resolve_endpoint "${BUILDER:-}" "$KURTOSIS_BUILDER_SERVICE" "rpc" "$LOCAL_BUILDER_FALLBACK")"
TX_PROXY="$(resolve_endpoint "${TX_PROXY:-}" "$KURTOSIS_TX_PROXY_SERVICE" "rpc" "$LOCAL_TX_PROXY_FALLBACK")"

PRIVATE_KEY=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
SEED="${SEED:-0x$(openssl rand -hex 32)}"

run_setup() {
    local scenario_file="$1"

    contender setup \
        -p "$PRIVATE_KEY" \
        "$scenario_file" \
        -r "$TX_PROXY" \
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
        -r "$TX_PROXY" \
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
