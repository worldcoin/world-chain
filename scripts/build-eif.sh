#!/bin/bash
set -euo pipefail

# Build the world-chain-proof-nitro-enclave EIF and emit its PCR measurements.
#
# Builds the enclave container image with Nix (see flake.nix), then converts
# it to an EIF with nitro-cli (built from source at a pinned tag so the EIF
# assembly itself is pinned). Runs on any Linux x86_64 host with Docker — Nitro
# hardware is only needed to *run* the enclave, not to build it.
#
# Usage: scripts/build-eif.sh [output-dir]   (default: target/eif)
#
# Outputs in <output-dir>:
#   world-chain-proof-nitro-enclave.eif   the enclave image
#   measurements.json               proofs/measurements.json with the freshly measured
#                                   PCR0/PCR1/PCR2 substituted into `.nitro`
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

# Nix builds the rootfs PCR0/PCR2 are measured over. Reproducible by construction — no apt
# state, no build timestamps, every path content-addressed — which is what makes the recorded
# PCRs re-derivable by anyone with this commit. There is deliberately no fallback: a second
# way to build the rootfs is a second set of PCRs.
echo "[1/3] Building enclave container image ($ENCLAVE_IMAGE_TAG)..."
command -v nix >/dev/null || {
  echo "[ERROR] nix not found. The enclave rootfs is built by flake.nix; there is no" >&2
  echo "        Dockerfile fallback, because a different rootfs means different PCRs." >&2
  exit 1
}
nix build .#enclave-image --no-link --print-out-paths > "$out_dir/enclave-image-path"
docker load -i "$(cat "$out_dir/enclave-image-path")"
docker tag world-chain-nitro-enclave:nix "$ENCLAVE_IMAGE_TAG"

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
eif_path="$out_dir/world-chain-proof-nitro-enclave.eif"
build_json="$out_dir/build-enclave.json"
NITRO_CLI_BLOBS="$nitro_cli_dir/blobs/x86_64" \
NITRO_CLI_ARTIFACTS="$out_dir/artifacts" \
  "$nitro_cli" build-enclave \
    --docker-uri "$ENCLAVE_IMAGE_TAG" \
    --output-file "$eif_path" | tee "$build_json"

# A whole measurements.json rather than the PCRs alone, so the output is directly
# comparable to the committed file: `diff <(jq -S . proofs/measurements.json) \
# <(jq -S . target/eif/measurements.json)` answers "does this checkout still measure to what
# it claims" in one command. The SP1 half is carried over untouched — it is measured by a
# different toolchain and nothing here is in a position to recompute it.
jq -S --slurpfile built <(jq '.Measurements' "$build_json") \
  '.nitro = {pcr0: ("0x" + $built[0].PCR0),
             pcr1: ("0x" + $built[0].PCR1),
             pcr2: ("0x" + $built[0].PCR2)}' \
  "$repo_root/proofs/measurements.json" > "$out_dir/measurements.json"

echo
echo "EIF:          $eif_path"
echo "Measurements: $out_dir/measurements.json"
jq . "$out_dir/measurements.json"
