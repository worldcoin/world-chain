#!/bin/bash
set -euo pipefail

# Build the world-chain-nitro-enclave EIF and emit its PCR measurements.
#
# Builds the enclave container image from proofs/nitro/Dockerfile, then converts
# it to an EIF with nitro-cli (built from source at a pinned tag so the EIF
# assembly itself is pinned). Runs on any Linux x86_64 host with Docker — Nitro
# hardware is only needed to *run* the enclave, not to build it.
#
# Usage: scripts/build-eif.sh [output-dir]   (default: target/eif)
#
# Outputs in <output-dir>:
#   world-chain-nitro-enclave.eif   the enclave image
#   pcrs.json                       PCR0/PCR1/PCR2 measurements
#
# Env overrides:
#   NITRO_CLI_VERSION   tag of aws/aws-nitro-enclaves-cli to build (default v1.4.2)
#   ENCLAVE_IMAGE_TAG   docker tag for the intermediate container image

if [ "$(uname -s)" != "Linux" ] || [ "$(uname -m)" != "x86_64" ]; then
  echo "[ERROR] EIF builds require Linux x86_64 (got $(uname -s)/$(uname -m))." >&2
  exit 1
fi

NITRO_CLI_VERSION="${NITRO_CLI_VERSION:-v1.4.2}"
ENCLAVE_IMAGE_TAG="${ENCLAVE_IMAGE_TAG:-world-chain-nitro-enclave:local}"

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

out_dir="${1:-target/eif}"
mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"

echo "[1/3] Building enclave container image ($ENCLAVE_IMAGE_TAG)..."
docker build -t "$ENCLAVE_IMAGE_TAG" -f proofs/nitro/Dockerfile .

echo "[2/3] Building nitro-cli $NITRO_CLI_VERSION..."
nitro_cli_dir="$out_dir/aws-nitro-enclaves-cli-$NITRO_CLI_VERSION"
nitro_cli="$nitro_cli_dir/target/release/nitro-cli"
if [ ! -x "$nitro_cli" ]; then
  rm -rf "$nitro_cli_dir"
  git clone --depth 1 --branch "$NITRO_CLI_VERSION" \
    https://github.com/aws/aws-nitro-enclaves-cli "$nitro_cli_dir"
  cargo build --release --bin nitro-cli --manifest-path "$nitro_cli_dir/Cargo.toml"
fi

echo "[3/3] Converting to EIF..."
eif_path="$out_dir/world-chain-nitro-enclave.eif"
build_json="$out_dir/build-enclave.json"
NITRO_CLI_BLOBS="$nitro_cli_dir/blobs/x86_64" \
NITRO_CLI_ARTIFACTS="$out_dir/artifacts" \
  "$nitro_cli" build-enclave \
    --docker-uri "$ENCLAVE_IMAGE_TAG" \
    --output-file "$eif_path" | tee "$build_json"

jq '.Measurements' "$build_json" > "$out_dir/pcrs.json"

echo
echo "EIF:  $eif_path"
echo "PCRs: $out_dir/pcrs.json"
jq . "$out_dir/pcrs.json"
