FROM public.ecr.aws/docker/library/rust:1.92.0-bookworm AS base

ARG FEATURES

RUN cargo install sccache --version ^0.9
RUN cargo install cargo-chef --version ^0.1

RUN apt-get update \
  && apt-get install -y clang libclang-dev gcc curl

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

RUN curl -L https://foundry.paradigm.xyz | bash && \
  /root/.foundry/bin/foundryup

ARG WORLD_CHAIN_BUILDER_BIN="world-chain"
ARG PROFILE="maxperf"
ARG FEATURES="jemalloc"

COPY --from=planner /app/recipe.json recipe.json

RUN --mount=type=cache,target=$SCCACHE_DIR,sharing=locked \
    cargo chef cook --profile ${PROFILE} --bin ${WORLD_CHAIN_BUILDER_BIN} --features ${FEATURES} --recipe-path recipe.json
COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=$SCCACHE_DIR,sharing=locked \
    cargo build --profile ${PROFILE} --features ${FEATURES} --bin ${WORLD_CHAIN_BUILDER_BIN}

# Deployments depend on sh wget and awscli v2
FROM public.ecr.aws/docker/library/debian:bookworm-slim
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

# Install s3fcp
RUN curl -L "https://github.com/Dzejkop/s3fcp/releases/download/v0.1.4/s3fcp-linux-x86_64" -o "/usr/local/bin/s3fcp" && \
  chmod +x /usr/local/bin/s3fcp

ARG WORLD_CHAIN_BUILDER_BIN="world-chain"
ARG PROFILE="maxperf"
COPY --from=builder /app/target/${PROFILE}/${WORLD_CHAIN_BUILDER_BIN} /usr/local/bin/

COPY --from=builder /root/.foundry/bin/cast /usr/local/bin/

COPY scripts/* /usr/local/bin

RUN chmod +x /usr/local/bin/*.sh

RUN cast --version

EXPOSE 30303 30303/udp 9001 8545 8546

ENTRYPOINT ["/usr/local/bin/world-chain"]
