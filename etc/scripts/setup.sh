#!/bin/bash
set -ex

L1_BEACON=$(kurtosis port print world-chain cl-1-teku-geth http --format "number")
L1_RPC=$(kurtosis port print world-chain el-1-geth-teku rpc --format "number")

cp .env.devnet .env

echo "L1_BEACON=http://127.0.0.1:$L1_BEACON" >> .env
echo "L1_RPC=http://127.0.0.1:$L1_RPC" >> .env

kurtosis files download world-chain op-deployer-configs \
    && cp op-deployer-configs/rollup-2151908.json network-config/rollup.json \
    && cp op-deployer-configs/genesis-2151908.json network-config/genesis.json \
    && rm -rf op-deployer-configs