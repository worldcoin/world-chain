# syntax=docker/dockerfile:1.7
#
# Container image used to produce the world-chain-proof-nitro-enclave EIF.
#
# Build:
#   docker build -t world-chain-proof-nitro-enclave \
#     -f proofs/backends/nitro/enclave/Dockerfile .
#
# Convert to an EIF:
#   nitro-cli build-enclave \
#     --docker-uri world-chain-proof-nitro-enclave:latest \
#     --output-file world-chain-proof-nitro-enclave.eif
#
# Or use scripts/build-eif.sh, which does both and emits the PCR measurements.
#
# The resulting EIF's PCR0/PCR1/PCR2 measurements must be copied into the verifier's
# `ExpectedPcrs` (see `proofs/workers/nitro/src/lib.rs`).
#
# REPRODUCIBILITY: the PCR measurements are a hash of the EIF contents, so this
# build must be bit-for-bit reproducible for anyone to re-derive the registered
# PCRs from source. Base images are pinned by digest, apt packages come from a
# fixed snapshot.debian.org timestamp, the final layer drops the files whose
# contents embed build-time state, and the image must be built via
# scripts/build-enclave-image.sh so BuildKit normalises every layer timestamp.
# When bumping any of those pins, expect the PCRs to change and re-register them.

# rust:1.95.0-bookworm
FROM rust@sha256:6258907abe69656e41cd992e0b705cdcfabcbbe3db374f92ed2d47121282d4a1 AS builder

# Pin the compiler to the image's toolchain: without this, rustup would honour
# the repo's rust-toolchain.toml copied into /src, so root toolchain bumps
# would silently rotate the PCRs.
ENV RUSTUP_TOOLCHAIN=1.95.0

# Pin apt to a fixed Debian snapshot so package versions cannot drift.
ARG DEBIAN_SNAPSHOT=20260601T000000Z
RUN rm -f /etc/apt/sources.list /etc/apt/sources.list.d/*.sources && \
    printf 'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian/%s bookworm main\ndeb [check-valid-until=no] http://snapshot.debian.org/archive/debian-security/%s bookworm-security main\n' \
        "$DEBIAN_SNAPSHOT" "$DEBIAN_SNAPSHOT" > /etc/apt/sources.list

# Dependencies for kona / rkyv / kzg-rs.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        clang \
        cmake \
        libssl-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

# Build from the enclave's standalone workspace so its own Cargo.lock (not the
# root workspace lock) pins every dependency version in the measured binary.
RUN cargo build \
        --release \
        --locked \
        --manifest-path proofs/backends/nitro/enclave/Cargo.toml \
        --bin world-chain-proof-nitro-enclave \
        --features enclave

# ---- runtime image (kept minimal so PCR0 stays stable) ---------------------
# debian:bookworm-slim
FROM debian@sha256:0104b334637a5f19aa9c983a91b54c89887c0984081f2068983107a6f6c21eeb

ARG DEBIAN_SNAPSHOT=20260601T000000Z
RUN rm -f /etc/apt/sources.list /etc/apt/sources.list.d/*.sources && \
    printf 'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian/%s bookworm main\ndeb [check-valid-until=no] http://snapshot.debian.org/archive/debian-security/%s bookworm-security main\n' \
        "$DEBIAN_SNAPSHOT" "$DEBIAN_SNAPSHOT" > /etc/apt/sources.list

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/proofs/backends/nitro/enclave/target/release/world-chain-proof-nitro-enclave \
     /usr/local/bin/world-chain-proof-nitro-enclave

ENV RUST_LOG=info
ENV NITRO_VSOCK_PORT=5005

ENTRYPOINT ["/usr/local/bin/world-chain-proof-nitro-enclave"]

# ---- reproducibility normalisation (must stay the last layer) --------------
# PCR2 hashes the user ramdisk linuxkit builds from this image's flattened rootfs
# (and PCR0 covers it too), so anything in the rootfs that varies per build
# rotates both. Drop the files whose *contents* embed build-time state:
#
#   * apt/dpkg logs record wall-clock times.
#   * ldconfig's aux-cache stores inode numbers, so it differs byte-for-byte
#     between two builds of identical source even at a fixed mtime. It is a
#     cache; ldconfig regenerates it on demand.
#
# File *timestamps* are handled outside the Dockerfile, by building with
# SOURCE_DATE_EPOCH and BuildKit's `rewrite-timestamp=true` (see
# scripts/build-enclave-image.sh) — a `touch` layer here cannot reliably
# override directory mtimes carried up from a lower overlayfs layer.
RUN rm -rf /var/log/apt /var/log/dpkg.log /var/log/alternatives.log \
        /var/cache/ldconfig
