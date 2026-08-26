#!/bin/bash
set -euo pipefail

# Build the world-chain-proof-nitro-enclave EIF and emit its PCR measurements.
#
# The EIF is assembled entirely inside Nix (see flake.nix): enclave binary,
# rootfs, ramdisks and EIF layout all come from pinned flake inputs, so the
# PCRs depend on nothing but the commit being built — no Docker daemon, no
# linuxkit, no nitro-cli. Any machine building the same commit measures the
# same values.
#
# Usage: scripts/build-eif.sh [output-dir]   (default: target/eif)
#
# Outputs in <output-dir>:
#   world-chain-proof-nitro-enclave.eif   the enclave image
#   pcr.json                        raw PCR output from eif_build
#   measurements.json               proofs/measurements.json with the freshly measured
#                                   PCR0/PCR1/PCR2 substituted into `.nitro`
#   enclave-image-path              store path of the OCI rootfs tarball (published for
#                                   provenance/local runs; not on the measured path)

if [ "$(uname -s)" != "Linux" ] || [ "$(uname -m)" != "x86_64" ]; then
  echo "[ERROR] EIF builds require Linux x86_64 (got $(uname -s)/$(uname -m))." >&2
  exit 1
fi

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

out_dir="${1:-target/eif}"
mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"

command -v nix >/dev/null || {
  echo "[ERROR] nix not found. The EIF is built by flake.nix; there is no fallback," >&2
  echo "        because a different build path means different PCRs." >&2
  exit 1
}

echo "[1/2] Building EIF..."
eif_store=$(nix build .#eif --no-link --print-out-paths)
install -m 0644 "$eif_store/image.eif" "$out_dir/world-chain-proof-nitro-enclave.eif"
install -m 0644 "$eif_store/pcr.json" "$out_dir/pcr.json"

echo "[2/2] Building enclave container image (publish artifact, not measured)..."
nix build .#enclave-image --no-link --print-out-paths > "$out_dir/enclave-image-path"

# A whole measurements.json rather than the PCRs alone, so the output is directly
# comparable to the committed file: `diff <(jq -S . proofs/measurements.json) \
# <(jq -S . target/eif/measurements.json)` answers "does this checkout still measure to what
# it claims" in one command. The SP1 half is carried over untouched — it is measured by a
# different toolchain and nothing here is in a position to recompute it.
jq -S --slurpfile built "$out_dir/pcr.json" \
  '.nitro = {pcr0: ("0x" + $built[0].PCR0),
             pcr1: ("0x" + $built[0].PCR1),
             pcr2: ("0x" + $built[0].PCR2)}' \
  "$repo_root/proofs/measurements.json" > "$out_dir/measurements.json"

echo
echo "EIF:          $out_dir/world-chain-proof-nitro-enclave.eif"
echo "Measurements: $out_dir/measurements.json"
jq . "$out_dir/measurements.json"
