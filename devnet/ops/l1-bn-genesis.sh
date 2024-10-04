#!/bin/sh

set -eu

echo "eth2-testnet-genesis path: $(which eth2-testnet-genesis)"

STATIC="/static"

eth2-testnet-genesis deneb \
  --config=${STATIC}/beacon-data/config.yml \
  --preset-phase0=minimal \
  --preset-altair=minimal \
  --preset-bellatrix=minimal \
  --preset-capella=minimal \
  --preset-deneb=minimal \
  --eth1-config=/static/genesis/genesis-l1.json \
  --state-output=/static/genesis/enesis-l1.ssz \
  --tranches-dir=/static/beacon-data/tranches \
  --mnemonics=mnemonics.yaml \
  --eth1-withdrawal-address=0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  --eth1-match-genesis-time
