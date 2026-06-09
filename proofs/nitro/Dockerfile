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
# The resulting EIF's PCR0/PCR1/PCR2 measurements must be copied into the verifier's
# `ExpectedPcrs` (see `proofs/nitro/src/lib.rs`).

ARG RUST_VERSION=1.95
FROM rust:${RUST_VERSION}-bookworm AS builder

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
FROM debian:bookworm-slim

# Install the NSM device tools required at runtime.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/world-chain-nitro-enclave \
     /usr/local/bin/world-chain-nitro-enclave

ENV RUST_LOG=info
ENV NITRO_VSOCK_PORT=5005

ENTRYPOINT ["/usr/local/bin/world-chain-nitro-enclave"]
