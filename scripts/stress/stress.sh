set -ex

BUILDER=${BUILDER:-http://localhost:8551}
TX_PROXY=${TX_PROXY:-http://localhost:8545}

PRIVATE_KEY=${PRIVATE_KEY:-$(echo "0x$(echo $(openssl rand -hex 32))")}

stress() {
    contender setup -p $PRIVATE_KEY "./stress/scenarios/stress.toml" -r $BUILDER --optimism
    contender spam --builder-url "$BUILDER" --txs-per-second ${TPS:-50} --duration ${DURATION:-600} --seed $(echo "0x$(echo $(openssl rand -hex 32))") -p $PRIVATE_KEY "./stress/scenarios/stress.toml" -r "$BUILDER" --optimism --min-balance 0.7eth
}

stress_precompile() {
    contender setup -p $PRIVATE_KEY "./stress/scenarios/precompileStress.toml" -r $BUILDER --optimism
    contender spam --builder-url "$BUILDER" --txs-per-second ${TPS:-50} --duration ${DURATION:-600} --seed $(echo "0x$(echo $(openssl rand -hex 32))") -p "$PRIVATE_KEY" './stress/scenarios/precompileStress.toml' -r "$BUILDER" --optimism --min-balance 0.7eth
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
