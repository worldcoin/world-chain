#!/bin/bash
# NOTE: This script should be run through `just devnet-up` rather than directly.
kurtosis_port=$(kurtosis port print world-chain wc-admin-world-chain-builder rpc)
BUILDER_SOCKET=$(echo "$kurtosis_port" | grep -o 'http://127.0.0.1:[0-9]*')
CHAIN_ID="2151908"

export PRIVATE_KEY=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80

forge script scripts/DeployDevnet.s.sol:DeployDevnet --rpc-url $BUILDER_SOCKET --chain $CHAIN_ID --broadcast