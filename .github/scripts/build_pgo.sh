#!/usr/bin/env bash
# PGO (Profile-Guided Optimization) build script for world-chain.
#
# This script automates the three-phase PGO workflow:
#   1. Build with PGO instrumentation
#   2. Collect profile data by running benchmarks
#   3. Rebuild with PGO optimization applied
#
# Environment variables:
#   FEATURES        - Cargo features (default: "jemalloc")
#   PGO_DATA_DIR    - Directory for raw PGO data (default: /tmp/pgo-data)
#   EXTRA_RUSTFLAGS - Additional RUSTFLAGS (default: "")
#   SKIP_COLLECT    - Skip collection phase, use existing data (default: false)
#   STRIP_SYMBOLS   - Strip debug symbols from final binary (default: false)
#
# Usage:
#   .github/scripts/build_pgo.sh

set -euo pipefail

FEATURES="${FEATURES:-jemalloc}"
PGO_DATA_DIR="${PGO_DATA_DIR:-/tmp/pgo-data}"
EXTRA_RUSTFLAGS="${EXTRA_RUSTFLAGS:-}"
SKIP_COLLECT="${SKIP_COLLECT:-false}"
STRIP_SYMBOLS="${STRIP_SYMBOLS:-false}"

BIN="world-chain"
PROFILE_BUILD="profiling"
PROFILE_FINAL="maxperf"

# Resolve llvm-profdata from the rustc sysroot
SYSROOT=$(rustc --print sysroot)
HOST_TRIPLE=$(rustc -vV | awk '/^host:/ { print $2 }')
LLVM_PROFDATA="$SYSROOT/lib/rustlib/$HOST_TRIPLE/bin/llvm-profdata"

if [[ ! -f "$LLVM_PROFDATA" ]]; then
    echo "ERROR: llvm-profdata not found. Install llvm-tools-preview:"
    echo "  rustup component add llvm-tools-preview"
    exit 1
fi

echo "=== Phase 1: PGO Instrumented Build ==="
mkdir -p "$PGO_DATA_DIR"

RUSTFLAGS="-Cprofile-generate=$PGO_DATA_DIR -C target-cpu=native $EXTRA_RUSTFLAGS" \
    cargo build --profile "$PROFILE_BUILD" --features "$FEATURES" --bin "$BIN"

echo "Instrumented binary: target/$PROFILE_BUILD/$BIN"

if [[ "$SKIP_COLLECT" != "true" ]]; then
    echo ""
    echo "=== Phase 2: Collect PGO Data ==="
    echo "Running benchmarks to collect profile data..."

    RUSTFLAGS="-Cprofile-generate=$PGO_DATA_DIR -C target-cpu=native $EXTRA_RUSTFLAGS" \
        cargo bench --profile "$PROFILE_BUILD" \
            --package world-chain-builder \
            --bench coordinator \
            -- --warm-up-time 3 --measurement-time 20

    echo "Profile data collected in: $PGO_DATA_DIR"
    echo "Files: $(find "$PGO_DATA_DIR" -name '*.profraw' | wc -l) .profraw files"
fi

echo ""
echo "=== Phase 3: Merge Profile Data ==="
"$LLVM_PROFDATA" merge -o "$PGO_DATA_DIR/merged.profdata" "$PGO_DATA_DIR"
echo "Merged profile: $PGO_DATA_DIR/merged.profdata"

echo ""
echo "=== Phase 4: PGO Optimized Build ==="
RUSTFLAGS="-Cprofile-use=$PGO_DATA_DIR/merged.profdata -C target-cpu=native $EXTRA_RUSTFLAGS" \
    cargo build --profile "$PROFILE_FINAL" --features "$FEATURES" --bin "$BIN"

if [[ "$STRIP_SYMBOLS" == "true" ]]; then
    echo "Stripping debug symbols..."
    strip "target/$PROFILE_FINAL/$BIN"
fi

echo ""
echo "=== Done ==="
echo "PGO-optimized binary: target/$PROFILE_FINAL/$BIN"
SIZE=$(ls -lh "target/$PROFILE_FINAL/$BIN" | awk '{print $5}')
echo "Binary size: $SIZE"
