set -ex

BUILDER=$(kurtosis port print world-chain op-el-builder-1-custom-op-node-op-kurtosis rpc)
TX_PROXY=$(kurtosis port print world-chain tx-proxy rpc)

stress() {
    PRIVATE_KEY=0x8b3a350cf5c34c9194ca85829a2df0ec3153be0318b5e2d3348e872092edffba
    contender setup -p $PRIVATE_KEY "./stress/scenarios/stress.toml" $TX_PROXY
    contender spam --builder-url "$BUILDER" --txs-per-second ${TPS:-100} --duration ${DURATION:-10} --seed $(echo "0x$(echo $(openssl rand -hex 32))") -p $PRIVATE_KEY "./stress/scenarios/stress.toml" "$TX_PROXY"
}

stress_precompile() {
    PRIVATE_KEY=0x8b3a350cf5c34c9194ca85829a2df0ec3153be0318b5e2d3348e872092edffba
    contender setup -p $PRIVATE_KEY "./stress/scenarios/precompileStress.toml" $TX_PROXY
    contender spam --builder-url "$BUILDER" --txs-per-second ${TPS:-100} --duration ${DURATION:-10} --seed $(echo "0x$(echo $(openssl rand -hex 32))") -p "$PRIVATE_KEY" './stress/scenarios/precompileStress.toml' "$TX_PROXY"
}

generate_report() {
    contender report "$TX_PROXY"
}

case "$1" in
"precompile-stress")
    stress_precompile
    ;;
"stress")
    stress
    ;;
"report")
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
