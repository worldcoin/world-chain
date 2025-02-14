#!/bin/bash
LOCAL_HOST="127.0.0.1"
PORT="8545"
CHAIN_ID="31337"

# start anvil in bg
anvil --chain-id ${CHAIN_ID} --block-time 2 --host ${LOCAL_HOST} --port ${PORT} &

ANVIL_PID=$!
echo "Anvil PID: $ANVIL_PID"
export PRIVATE_KEY=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80

# Safe EOA Signers
# Mnemonic: "test test test test test test test test test test test junk"
export SAFE_OWNER_0=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 # m/44'/60'/0'/0/0
export SAFE_OWNER_1=0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d # m/44'/60'/0'/0/1
export SAFE_OWNER_2=0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a # m/44'/60'/0'/0/2
export SAFE_OWNER_3=0x7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6 # m/44'/60'/0'/0/3
export SAFE_OWNER_4=0x47e179ec197488593b187f80a00eb0da91f1b9d0b13f8733639f19c30a34926a # m/44'/60'/0'/0/4
export SAFE_OWNER_5=0x8b3a350cf5c34c9194ca85829a2df0ec3153be0318b5e2d3348e872092edffba # m/44'/60'/0'/0/5

# Deploy contracts
forge script scripts/DeployDevnet.s.sol:DeployDevnet --rpc-url http://127.0.0.1:8545 --broadcast

# Wait for anvil to finish
wait $ANVIL_PID