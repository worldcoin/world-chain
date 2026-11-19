# syntax=docker/dockerfile:1.10
FROM public.ecr.aws/docker/library/rust:1.99.0-bookworm AS base

RUN --mount=type=cache,target=/usr/local/cargo/registry \
  --mount=type=cache,target=/usr/local/cargo/git \
  cargo install --locked sccache --version ^0.9 \
  && cargo install --locked cargo-chef --version ^0.1

RUN apt-get update \
  && apt-get install -y --no-install-recommends clang libclang-dev gcc curl \
  && rm -rf /var/lib/apt/lists/*

ENV CARGO_HOME=/usr/local/cargo \
    RUSTC_WRAPPER=sccache \
    SCCACHE_DIR=/sccache

FROM base AS planner
WORKDIR /app

COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry \
  --mount=type=cache,target=/usr/local/cargo/git \
  --mount=type=cache,target=$SCCACHE_DIR,sharing=locked \
  cargo chef prepare --recipe-path recipe.json

FROM base AS builder
WORKDIR /app

# cast ships in the runtime image; 1.8.6+ carries the eMudhra distrust (rustls-webpki 0.103.16).
RUN curl -L https://foundry.paradigm.xyz | bash && \
  /root/.foundry/bin/foundryup --install 1.8.6


ARG WORLD_CHAIN_BUILDER_BIN="world-chain"
ARG PROFILE="maxperf"
ARG FEATURES="jemalloc"
ARG SCCACHE_BUCKET
ARG SCCACHE_REGION
ARG SCCACHE_S3_KEY_PREFIX

COPY --from=planner /app/recipe.json recipe.json

# The measured crates are standalone workspaces (excluded from the root workspace), which
# cargo-chef's recipe cannot reconstruct — cook needs their real sources to resolve the
# root workspace's path dependencies on them.
COPY proofs/measured proofs/measured

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=$SCCACHE_DIR,sharing=locked \
    --mount=type=secret,id=aws_access_key_id,env=AWS_ACCESS_KEY_ID \
    --mount=type=secret,id=aws_secret_access_key,env=AWS_SECRET_ACCESS_KEY \
    --mount=type=secret,id=aws_session_token,env=AWS_SESSION_TOKEN \
    if [ -z "$SCCACHE_BUCKET" ]; then unset SCCACHE_BUCKET SCCACHE_REGION SCCACHE_S3_KEY_PREFIX; fi && \
    cargo chef cook --locked --profile ${PROFILE} --bin ${WORLD_CHAIN_BUILDER_BIN} --features ${FEATURES} --recipe-path recipe.json

ARG VERGEN_GIT_SHA
COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=$SCCACHE_DIR,sharing=locked \
    --mount=type=secret,id=aws_access_key_id,env=AWS_ACCESS_KEY_ID \
    --mount=type=secret,id=aws_secret_access_key,env=AWS_SECRET_ACCESS_KEY \
    --mount=type=secret,id=aws_session_token,env=AWS_SESSION_TOKEN \
    if [ -z "$SCCACHE_BUCKET" ]; then unset SCCACHE_BUCKET SCCACHE_REGION SCCACHE_S3_KEY_PREFIX; fi && \
    cargo build --locked --profile ${PROFILE} --features ${FEATURES} --bin ${WORLD_CHAIN_BUILDER_BIN}

# Deployments depend on sh wget and awscli v2
FROM public.ecr.aws/docker/library/debian:bookworm-slim AS runtime-tools
WORKDIR /app

# Install runtime tools and require Debian's eMudhra distrust update.
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
  dpkg --compare-versions "$(dpkg-query -W -f='${Version}' ca-certificates)" ge '20250419~deb12u2' && \
  apt-get clean && \
  rm -rf /var/lib/apt/lists/*

# Install AWS CLI v2
ARG TARGETARCH
RUN case "$TARGETARCH" in \
    amd64) aws_cli_arch=x86_64 ;; \
    arm64) aws_cli_arch=aarch64 ;; \
    *) echo "Unsupported AWS CLI target architecture: $TARGETARCH" >&2; exit 1 ;; \
  esac && \
  curl --fail --show-error --location --connect-timeout 10 --max-time 300 \
    "https://awscli.amazonaws.com/awscli-exe-linux-${aws_cli_arch}.zip" -o "/tmp/awscliv2.zip" && \
  unzip /tmp/awscliv2.zip -d /tmp && \
  /tmp/aws/install && \
  rm -rf /tmp/aws /tmp/awscliv2.zip && \
  aws --version

# The AWS CLI bundles its own CA store; use Debian's, which distrusts the eMudhra roots.
ENV AWS_CA_BUNDLE=/etc/ssl/certs/ca-certificates.crt

# Bookworm's musl lacks current security fixes; pin the patched loader separately.
FROM public.ecr.aws/docker/library/alpine:3.24@sha256:f59abe656b692d901e184be3445e084786602108599d2a51d229c0f5a48c71bf AS cannon-runtime
RUN apk --no-network info --installed 'musl>=1.2.6-r3'

# Supply --build-context qualified-payload=<assembled platform payload>.
FROM scratch AS qualified-payload

FROM runtime-tools AS distribution-tools
RUN groupadd --gid 10001 worldchain && \
  useradd --uid 10001 --gid 10001 --create-home worldchain && \
  chown 10001:10001 /app
ENV HOME=/home/worldchain

FROM distribution-tools AS distribution-world-chain
COPY --from=qualified-payload --chmod=0755 /bin/world-chain /bin/cast /usr/local/bin/
COPY --from=qualified-payload /licenses/ /usr/share/licenses/world-chain/
COPY --from=qualified-payload /distribution-inputs.json /usr/share/world-chain/distribution-inputs.json
COPY scripts/* /usr/local/bin/
RUN case "$TARGETARCH" in amd64) machine=62 ;; arm64) machine=183 ;; *) exit 1 ;; esac && \
  for name in world-chain cast; do \
    test "$(od -An -tx1 -N6 /usr/local/bin/$name | tr -d ' \n')" = 7f454c460201 && \
    test "$(od -An -tu2 -j18 -N2 /usr/local/bin/$name)" -eq "$machine" || \
    { echo "Invalid ELF for $TARGETARCH: $name" >&2; exit 1; }; \
  done && chmod +x /usr/local/bin/*.sh && world-chain --version && cast --version
EXPOSE 30303 30303/udp 9001 8545 8546
USER 10001:10001
ENTRYPOINT ["/usr/local/bin/world-chain"]

FROM distribution-tools AS distribution-op
RUN apt-get -o Acquire::Retries=2 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 update && \
  apt-get -o Acquire::Retries=2 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 \
  install -y --no-install-recommends libstdc++6 libudev1 && \
  apt-get clean && rm -rf /var/lib/apt/lists/*
COPY --from=cannon-runtime /lib/ld-musl-*.so.1 /lib/libc.musl-*.so.1 /lib/
COPY --from=qualified-payload --chmod=0755 \
  /bin/op-node /bin/op-batcher /bin/op-proposer /bin/op-conductor \
  /bin/op-challenger /bin/op-deployer /bin/op-dispute-mon \
  /bin/cannon /bin/kona-host /bin/forge /bin/cast /usr/local/bin/
COPY --from=qualified-payload /licenses/ /usr/share/licenses/world-chain/
COPY --from=qualified-payload /distribution-inputs.json /usr/share/world-chain/distribution-inputs.json
RUN case "$TARGETARCH" in amd64) machine=62 ;; arm64) machine=183 ;; *) exit 1 ;; esac && \
  for name in op-node op-batcher op-proposer op-conductor op-challenger op-deployer \
    op-dispute-mon cannon kona-host forge cast; do \
    test "$(od -An -tx1 -N6 /usr/local/bin/$name | tr -d ' \n')" = 7f454c460201 && \
    test "$(od -An -tu2 -j18 -N2 /usr/local/bin/$name)" -eq "$machine" || \
    { echo "Invalid ELF for $TARGETARCH: $name" >&2; exit 1; }; \
  done && op-node --version && op-deployer --version && forge --version && cast --version
USER 10001:10001
ENTRYPOINT ["/usr/local/bin/op-node"]

FROM distribution-op AS distribution-combined
USER 0
COPY --from=qualified-payload --chmod=0755 /bin/world-chain /usr/local/bin/
COPY scripts/* /usr/local/bin/
RUN case "$TARGETARCH" in amd64) machine=62 ;; arm64) machine=183 ;; *) exit 1 ;; esac && \
  test "$(od -An -tx1 -N6 /usr/local/bin/world-chain | tr -d ' \n')" = 7f454c460201 && \
  test "$(od -An -tu2 -j18 -N2 /usr/local/bin/world-chain)" -eq "$machine" && \
  chmod +x /usr/local/bin/*.sh && world-chain --version
EXPOSE 30303 30303/udp 9001 8545 8546
USER 10001:10001
ENTRYPOINT ["/usr/local/bin/world-chain"]

FROM runtime-tools AS runtime

ARG WORLD_CHAIN_BUILDER_BIN="world-chain"
ARG PROFILE="maxperf"
COPY --from=builder /app/target/${PROFILE}/${WORLD_CHAIN_BUILDER_BIN} /usr/local/bin/

COPY --from=builder /root/.foundry/bin/cast /usr/local/bin/

COPY scripts/* /usr/local/bin

RUN chmod +x /usr/local/bin/*.sh

RUN cast --version

EXPOSE 30303 30303/udp 9001 8545 8546

ENTRYPOINT ["/usr/local/bin/world-chain"]
