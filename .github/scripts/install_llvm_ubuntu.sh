#!/usr/bin/env bash
set -euo pipefail

# apt runs downloads as the unprivileged `_apt` user. Keep repository keys and
# source files readable even when this script is called from a restrictive
# parent environment.
umask 022

version="${1:-22}"
bins=(clang llvm-config lld ld.lld FileCheck)

# Install prerequisites for the official apt.llvm.org installer.
# software-properties-common is needed on Debian bookworm and older Ubuntu
# releases; newer distributions use a different path in llvm.sh.
apt-get update -qq
apt-get install -y --no-install-recommends \
    ca-certificates \
    gnupg \
    lsb-release \
    wget
apt-get install -y --no-install-recommends software-properties-common 2>/dev/null || true

llvm_installer="$(mktemp)"
trap 'rm -f "$llvm_installer"' EXIT

wget -qO "$llvm_installer" https://apt.llvm.org/llvm.sh
chmod +x "$llvm_installer"
"$llvm_installer" "$version" all

# llvm.sh creates this key on Debian/Ubuntu. Make its required permissions
# explicit for later apt operations as an additional safeguard.
if [[ -f /etc/apt/trusted.gpg.d/apt.llvm.org.asc ]]; then
    chmod 0644 /etc/apt/trusted.gpg.d/apt.llvm.org.asc
fi

for bin in "${bins[@]}"; do
    versioned_bin="$(command -v "$bin-$version" || true)"
    if [[ -z "$versioned_bin" ]]; then
        echo "warning: $bin-$version not found" >&2
        continue
    fi
    ln -fs "$versioned_bin" "/usr/bin/$bin"
done

llvm_prefix="/usr/lib/llvm-$version"
if [[ ! -x "$llvm_prefix/bin/llvm-config" ]]; then
    echo "LLVM $version was not installed under $llvm_prefix" >&2
    exit 1
fi

echo "LLVM $version installed at $llvm_prefix:"
"$llvm_prefix/bin/llvm-config" --version
