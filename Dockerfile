FROM rust:1.89.0-bookworm AS base

ARG FEATURES
ARG BUILD_PROFILE=dev

RUN cargo install sccache --version ^0.9
RUN cargo install cargo-chef --version ^0.1

RUN apt-get update \
    && apt-get install -y clang libclang-dev gcc

ENV CARGO_HOME=/usr/local/cargo
ENV RUSTC_WRAPPER=sccache
ENV SCCACHE_DIR=/sccache

FROM base AS planner
WORKDIR /app

COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=$SCCACHE_DIR,sharing=locked \
    cargo chef prepare --recipe-path recipe.json

FROM base AS builder
WORKDIR /app

RUN cargo install --git https://github.com/foundry-rs/foundry --tag v1.3.6 --profile release --locked cast

ARG WORLD_CHAIN_BUILDER_BIN="world-chain"
ARG BUILD_PROFILE=dev
COPY --from=planner /app/recipe.json recipe.json

RUN --mount=type=cache,target=$SCCACHE_DIR,sharing=locked \
    cargo chef cook --profile ${BUILD_PROFILE} --features jemalloc --bin ${WORLD_CHAIN_BUILDER_BIN} --recipe-path recipe.json

COPY . .

ARG BUILD_PROFILE=dev
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=$SCCACHE_DIR,sharing=locked \
    cargo build --profile ${BUILD_PROFILE} --features jemalloc --bin ${WORLD_CHAIN_BUILDER_BIN}

# Deployments depend on sh wget and awscli v2
FROM debian:bookworm-slim
WORKDIR /app

# Install wget in the final image
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates \
        unzip \
        curl \
        lz4 \
        wget \
        jq \
        netcat-traditional \
        tzdata && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/*

# Install AWS CLI v2
RUN curl "https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip" -o "/tmp/awscliv2.zip" && \
    unzip /tmp/awscliv2.zip -d /tmp && \
    /tmp/aws/install && \
    rm -rf /tmp/aws /tmp/awscliv2.zip

ARG WORLD_CHAIN_BUILDER_BIN="world-chain"
ARG BUILD_PROFILE=dev

# Copy binary from the correct profile directory
# Note: Cargo uses 'debug' directory for 'dev' profile
RUN --mount=type=bind,from=builder,source=/app/target,target=/tmp/target \
    if [ "${BUILD_PROFILE}" = "dev" ]; then \
        cp /tmp/target/debug/${WORLD_CHAIN_BUILDER_BIN} /usr/local/bin/; \
    else \
        cp /tmp/target/${BUILD_PROFILE}/${WORLD_CHAIN_BUILDER_BIN} /usr/local/bin/; \
    fi

COPY --from=builder /usr/local/cargo/bin/cast /usr/local/bin/
RUN cast --version

EXPOSE 30303 30303/udp 9001 8545 8546

ENTRYPOINT ["/usr/local/bin/world-chain"]
