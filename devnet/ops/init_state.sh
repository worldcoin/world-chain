
echo "Generating Beacon Data"
set -exu
VERBOSITY=${GETH_VERBOSITY:-3}
GETH_DATA_DIR=/db
GETH_CHAINDATA_DIR="$GETH_DATA_DIR/geth/chaindata"
MOUNT="/static"
export GENESIS_MOUNT_PATH="${MOUNT}/genesis"
export BEACON_DATA_MOUNT_PATH="${MOUNT}/beacon"
export AUTH_MOUNT_PATH="${MOUNT}/auth"
RPC_PORT="${RPC_PORT:-8545}"
WS_PORT="${WS_PORT:-8546}"
GENESIS_FILE_PATH="${GENESIS_MOUNT_PATH}/genesis-l1.json"

if [ ! -d "$GETH_CHAINDATA_DIR" ]; then
	echo "Generating genesis file"
	TIME="$(date +%s)"
	HEX_TIME="$(echo $TIME | xargs printf "0x%x")"
	sed -i "s/\"timestamp\": \".*\"/\"timestamp\": \"$HEX_TIME\"/" "${GENESIS_MOUNT_PATH}/genesis-l1.json"
	sed -i "s/\"timestamp\": \".*\"/\"timestamp\": \"$HEX_TIME\"/" "${GENESIS_MOUNT_PATH}/genesis-l2.json"
	sed -i "s/\"l2_time\": .*,/\"l2_time\": $TIME,/" ${GENESIS_MOUNT_PATH}/rollup.json

	go install github.com/protolambda/eth2-testnet-genesis@latest
	go install github.com/protolambda/eth2-val-tools@latest
	export PATH=$PATH:/root/go/bin

	eth2-val-tools keystores --source-min=0 --source-max=64 \
		--source-mnemonic="test test test test test test test test test test test junk" --out-loc=$BEACON_DATA_MOUNT_PATH/data
	rm -rf $BEACON_DATA_MOUNT_PATH/data/lodestar-secrets $BEACON_DATA_MOUNT_PATH/data/nimbus-keys $BEACON_DATA_MOUNT_PATH/data/prysm $BEACON_DATA_MOUNT_PATH/data/teku-keys $BEACON_DATA_MOUNT_PATH/data/teku-secrets

	echo "Generating Beacon Genesis"

	eth2-testnet-genesis deneb \
		--config=${BEACON_DATA_MOUNT_PATH}/config.yaml \
		--preset-phase0=minimal \
		--preset-altair=minimal \
		--preset-bellatrix=minimal \
		--preset-capella=minimal \
		--preset-deneb=minimal \
		--eth1-config=${GENESIS_MOUNT_PATH}/genesis-l1.json \
		--state-output=${GENESIS_MOUNT_PATH}/genesis-l1.ssz \
		--tranches-dir=${BEACON_DATA_MOUNT_PATH}/tranches \
		--mnemonics=${AUTH_MOUNT_PATH}/mnemonics.yaml \
		--eth1-withdrawal-address=0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
		--eth1-match-genesis-time

	echo "$GETH_CHAINDATA_DIR missing, running init"
	echo "Initializing genesis."
	geth --verbosity="$VERBOSITY" init \
		--datadir="$GETH_DATA_DIR" \
		--state.scheme=hash \
		$GENESIS_FILE_PATH
else
	echo "$GETH_CHAINDATA_DIR exists."
fi
