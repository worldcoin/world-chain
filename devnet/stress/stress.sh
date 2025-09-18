set -ex

BUILDER=$(kurtosis port print world-chain op-el-builder-2151908-1-custom-op-node-op-kurtosis rpc)
TX_PROXY=$(kurtosis port print world-chain tx-proxy rpc)
PRIVATE_KEY=0x8b3a350cf5c34c9194ca85829a2df0ec3153be0318b5e2d3348e872092edffba

stress() {
    contender setup -p $PRIVATE_KEY "./stress/scenarios/stress.toml" -r $BUILDER --optimism
    contender spam --builder-url "$BUILDER" --txs-per-second ${TPS:-50} --duration ${DURATION:-10} --seed $(echo "0x$(echo $(openssl rand -hex 32))") -p $PRIVATE_KEY "./stress/scenarios/stress.toml" -r "$BUILDER" --optimism --min-balance 1
}

stress_precompile() {
    contender setup -p $PRIVATE_KEY "./stress/scenarios/precompileStress.toml" -r $BUILDER --optimism
    contender spam --builder-url "$BUILDER" --txs-per-second ${TPS:-50} --duration ${DURATION:-10} --seed $(echo "0x$(echo $(openssl rand -hex 32))") -p "$PRIVATE_KEY" './stress/scenarios/precompileStress.toml' -r "$BUILDER" --optimism --min-balance 1
}

generate_report() {
    contender report "$BUILDER"
}

case "$1" in
"stress-precompile")
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
