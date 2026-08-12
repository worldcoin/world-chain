#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <vkeys.json> <elf-directory>" >&2
  exit 2
fi

manifest=$1
elf_dir=$2

expected_hash() {
  local program=$1
  local hash
  hash="$(sed -n "/\"${program}\"/,/}/ s/.*\"sha256\": \"\([0-9a-f]\{64\}\)\".*/\1/p" "${manifest}")"
  if [[ ! "${hash}" =~ ^[0-9a-f]{64}$ ]]; then
    echo "failed to read ${program} SHA-256 from ${manifest}" >&2
    exit 1
  fi
  printf '%s' "${hash}"
}

verify_elf() {
  local program=$1
  local filename=$2
  local expected actual
  expected="$(expected_hash "${program}")"
  actual="$(sha256sum "${elf_dir}/${filename}" | cut -d ' ' -f 1)"
  if [[ "${actual}" != "${expected}" ]]; then
    echo "${program} ELF hash mismatch: expected ${expected}, got ${actual}" >&2
    exit 1
  fi
  echo "verified ${program} ELF ${actual}"
}

verify_elf world-chain-range-ethereum world-chain-proof-succinct-range-ethereum
verify_elf world-chain-aggregation world-chain-proof-succinct-aggregation
