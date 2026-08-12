#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
programs_root="${repo_root}/proofs/backends/sp1/programs"
target_dir="${programs_root}/target/elf-compilation/docker"
separator=$'\x1f'

export CARGO_TARGET_DIR="${target_dir}"
export RUSTUP_TOOLCHAIN=succinct
export RUSTC_BOOTSTRAP=1
export RUSTC="$(rustc --print sysroot)/bin/rustc"
export CFLAGS_riscv32im_succinct_zkvm_elf=-D__ILP32__
export CARGO_ENCODED_RUSTFLAGS="-C${separator}passes=lower-atomic${separator}-C${separator}link-arg=--image-base=2013265920${separator}-C${separator}panic=abort${separator}--cfg${separator}getrandom_backend=\"custom\"${separator}-C${separator}llvm-args=-misched-prera-direction=bottomup${separator}-C${separator}llvm-args=-misched-postra-direction=bottomup"

for program in range-ethereum aggregation; do
  cargo build \
    --locked \
    --release \
    --target riscv64im-succinct-zkvm-elf \
    --ignore-rust-version \
    --manifest-path "${programs_root}/${program}/Cargo.toml"
done
