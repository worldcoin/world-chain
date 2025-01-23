#!/bin/bash
# Deploys the WorldID contract to the Devnet in a scrappy way (because of Solc issues)
SOCKET = $1
CHAIN_ID = $2
PRIVATE_KEY = $3

export PRIVATE_KEY=$PRIVATE_KEY

git clone https://github.com/worldcoin/world-id-state-bridge.git /tmp/world-id-state-bridge

cd /tmp/world-id-state-bridge && \
    source ~/.profile && \
    nvm use v22 && \
    yarn install && \

cd /tmp/world-id-state-bridge && \
    source ~/.profile && \
    nvm use v22 

