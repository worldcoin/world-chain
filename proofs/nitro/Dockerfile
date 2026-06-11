# syntax=docker/dockerfile:1.7
#
# Container image used to produce the world-chain-nitro-enclave EIF.
#
# Build:
#   docker build -t world-chain-nitro-enclave \
#     -f proofs/nitro/Dockerfile .
#
# Convert to an EIF:
#   nitro-cli build-enclave \
#     --docker-uri world-chain-nitro-enclave:latest \
#     --output-file world-chain-nitro-enclave.eif
#
# Or use scripts/build-eif.sh, which does both and emits the PCR measurements.
#
# The resulting EIF's PCR0/PCR1/PCR2 measurements must be copied into the verifier's
# `ExpectedPcrs` (see `proofs/nitro/src/lib.rs`).
#
# REPRODUCIBILITY: the PCR measurements are a hash of the EIF contents, so this
# build must be bit-for-bit reproducible for anyone to re-derive the registered
# PCRs from source. Base images are pinned by digest and apt packages come from
# a fixed snapshot.debian.org timestamp. When bumping either pin, expect the
# PCRs to change and re-register them.

# rust:1.95.0-bookworm
FROM rust@sha256:6258907abe69656e41cd992e0b705cdcfabcbbe3db374f92ed2d47121282d4a1 AS builder

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

RUN cargo build \
        --release \
        --locked \
        --bin world-chain-nitro-enclave \
        -p world-chain-proof-nitro \
        --features enclave-bin

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

COPY --from=builder /src/target/release/world-chain-nitro-enclave \
     /usr/local/bin/world-chain-nitro-enclave

ENV RUST_LOG=info
ENV NITRO_VSOCK_PORT=5005

ENTRYPOINT ["/usr/local/bin/world-chain-nitro-enclave"]
