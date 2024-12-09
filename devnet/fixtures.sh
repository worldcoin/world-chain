#!/bin/bash
set -e
MNEMONIC="test test test test test test test test test test test junk"
export IDENTITY=11ff11
export INCLUSION_PROOF_URL="https://signup-orb-ethereum.stage-crypto.worldcoin.dev/inclusionProof"
kurtosis_port=$(kurtosis port print world-chain wc-admin-world-chain-builder rpc)
BUILDER_SOCKET=$(echo "$kurtosis_port" | grep -o 'http://127.0.0.1:[0-9]*')

make_txs() {
    local fixture_file=$1
    declare -a local_fixture=()
    for t in {0..29}; do
        tx=$(cast mktx --private-key "$2" 0xDeaDbeefdEAdbeefdEadbEEFdeadbeEFdEaDbeeF --value $3 --rpc-url "$BUILDER_SOCKET" --nonce "$t")
        transaction=$(toolkit prove -t "$tx" -N "$t")
        local_fixture[$t]="$transaction"
    done

    # Write local_fixture to a temporary file
    printf '%s\n' "${local_fixture[@]}" >"$fixture_file"
    echo "Finished Process: $3"
}

# Configure for more signer's
for x in {0..0}; do
    echo "Started Process: $x"
    signer=($(echo $(cast wallet private-key --mnemonic "${MNEMONIC}" --mnemonic-index $x)))
    tmp_file="/tmp/fixture_$x.tmp"
    make_txs "$tmp_file" "$signer" "$x" &
done

wait

# collect all temporary fixture files and combine them into the FIXTURE array
FIXTURE=()
for tmp_file in /tmp/fixture_*.tmp; do
    while read -r line; do
        FIXTURE+=("$line")
    done <"$tmp_file"
done

# Convert the FIXTURE array into a JSON-friendly format
fixture_json=$(printf '%s\n' "${FIXTURE[@]}" | jq -R . | jq -s .)

# Write the fixture to a JSON file
echo "{\"fixture\": $fixture_json}" >"${PWD}/fixtures/fixture.json"

# Clean up temporary files
rm /tmp/fixture_*.tmp
