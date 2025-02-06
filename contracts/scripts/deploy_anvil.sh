#!/bin/bash
LOCAL_HOST="127.0.0.1"
PORT="8545"
CHAIN_ID="31337"

# start anvil in bg
anvil --chain-id ${CHAIN_ID} --block-time 2 --host ${LOCAL_HOST} --port ${PORT} &

ANVIL_PID=$!
echo "Anvil PID: $ANVIL_PID"
export PRIVATE_KEY=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80

# Deploy contracts
forge script scripts/DeployDevnet.s.sol:DeployDevnet --rpc-url http://127.0.0.1:8545 --broadcast

# Wait for anvil to finish
wait $ANVIL_PID