FROM rust:latest

COPY . ./world-chain-builder

WORKDIR /world-chain-builder

RUN apt-get update && apt-get install -y libclang-dev gcc 

RUN cargo install --path . --locked

ENTRYPOINT [ "world-chain-builder" ]