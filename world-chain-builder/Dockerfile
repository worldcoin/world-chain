FROM rust:1.82.0

COPY . ./world-chain-builder

WORKDIR /world-chain-builder

RUN apt-get update && apt-get install -y libclang-dev gcc

RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/world-chain-builder/target \
    cargo install --path crates/world/bin --locked

ENTRYPOINT [ "world-chain-builder" ]
