FROM rust:1.81.0

COPY . ./genesis

WORKDIR /genesis

RUN apt-get update && apt-get install -y libclang-dev gcc

RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/genesis/target \
    cargo install --path . --locked

ENTRYPOINT [ "genesis" ]