#!/bin/bash
set -exu

VERBOSITY=${GETH_VERBOSITY:-3}
GETH_DATA_DIR=/db
GETH_CHAINDATA_DIR="$GETH_DATA_DIR/geth/chaindata"
MOUNT="/static"
GENESIS_MOUNT_PATH="${MOUNT}/genesis"
BEACON_DATA_MOUNT_PATH="${MOUNT}/beacon"
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

	echo "eth2-testnet-genesis path: $(which eth2-testnet-genesis)"
	go install github.com/protolambda/eth2-testnet-genesis@latest 
	export PATH=$PATH:/root/go/bin
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
		--mnemonics=${MOUNT}/auth/mnemonics.yaml \
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

# Warning: Archive mode is required, otherwise old trie nodes will be
# pruned within minutes of starting the devnet.

exec geth \
	--datadir="$GETH_DATA_DIR" \
	--verbosity="$VERBOSITY" \
	--http \
	--http.corsdomain="*" \
	--http.vhosts="*" \
	--http.addr=0.0.0.0 \
	--http.port="$RPC_PORT" \
	--http.api=web3,debug,eth,txpool,net,engine \
	--ws \
	--ws.addr=0.0.0.0 \
	--ws.port="$WS_PORT" \
	--ws.origins="*" \
	--ws.api=debug,eth,txpool,net,engine \
	--syncmode=full \
	--maxpeers=1 \
	--rpc.allow-unprotected-txs \
	--authrpc.addr="0.0.0.0" \
	--authrpc.port="8551" \
	--authrpc.vhosts="*" \
	--authrpc.jwtsecret=${MOUNT}/auth/jwt.txt \
	--gcmode=archive \
	--state.scheme=hash \
	--metrics \
	--metrics.addr=0.0.0.0 \
	--metrics.port=6060 \
	"$@"
