#!/bin/bash
set -ex

L1_BEACON=$(kurtosis port print world-chain cl-1-teku-geth http --format "number")
L1_RPC=$(kurtosis port print world-chain el-1-geth-teku rpc --format "number")

echo "L1_BEACON=http://127.0.0.1:$L1_BEACON" >> .env
echo "L1_RPC=http://127.0.0.1:$L1_RPC" >> .env

kurtosis files download world-chain op-deployer-configs \
    && cp op-deployer-configs/rollup-2151908.json network-config/rollup.json \
    && cp op-deployer-configs/genesis-2151908.json network-config/genesis.json \
    && rm -rf op-deployer-configs

L2_TCP_DISCOVERY=$(kurtosis port print world-chain op-el-1-op-geth-op-node-op-kurtosis tcp-discovery --format "number")
L2_RPC_PORT=$(kurtosis port print world-chain op-el-1-op-geth-op-node-op-kurtosis rpc --format "number")

# Fetch the enode from the sequencer
ENODE_PREFIX=$(curl --data '{"method":"admin_nodeInfo","params":[],"id":1,"jsonrpc":"2.0"}' \
    -H "Content-Type: application/json" -X POST localhost:$L2_RPC_PORT | jq -r '.result.enode' | sed -nE 's/^(enode:\/\/[0-9a-fA-F]+@).*$/\1/p')
ENODE="${ENODE_PREFIX}127.0.0.1:$L2_TCP_DISCOVERY"

echo "L2_ENODE=$ENODE" >> .env

HOST_IP=$(ifconfig | sed -nE 's/127.0.0.1//;s/.*inet (addr:)?(([0-9]*\.){3}[0-9]*).*/\2/p')
echo "HOST_IP=$HOST_IP" >> .env

