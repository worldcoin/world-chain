#!/bin/bash
set -euo pipefail

# Build the enclave container image reproducibly and load it into the local docker store.
#
# Shared by scripts/build-eif.sh and the reproducibility probe so both exercise the exact
# same build path. Two things make the resulting rootfs bit-for-bit stable:
#
#   * SOURCE_DATE_EPOCH + rewrite-timestamp=true — BuildKit rewrites every layer's
#     timestamps to the epoch. This covers directory mtimes that a `touch` layer inside the
#     Dockerfile cannot reliably override (overlayfs does not always carry a directory's
#     metadata-only change into the exported rootfs).
#   * The Dockerfile drops the files whose *contents* embed build-time state
#     (apt/dpkg logs, ldconfig's aux-cache, which stores inode numbers).
#
# Usage: scripts/build-enclave-image.sh <image-tag>

tag="${1:?usage: build-enclave-image.sh <image-tag> [extra docker buildx flags]}"
shift

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

# Keep in sync with SOURCE_DATE_EPOCH in the enclave Dockerfile; changing it rotates the PCRs.
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-1780272000}"

docker buildx build \
  --build-arg "SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH}" \
  --output "type=docker,name=${tag},rewrite-timestamp=true" \
  -f proofs/backends/nitro/enclave/Dockerfile \
  "$@" \
  .
