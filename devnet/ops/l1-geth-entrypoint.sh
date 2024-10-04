#!/bin/bash
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
