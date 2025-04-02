set positional-arguments

# default recipe to display help information
default:
    @just --list

# Spawns the devnet
devnet-up: deploy-devnet deploy-contracts

deploy-devnet: build
    @just ./devnet/devnet-up

build:
    docker buildx build -t world-chain-builder:latest .

deploy-contracts:
    @just ./contracts/deploy-contracts

# Stops the devnet **This will prune all docker containers**
devnet-down:
    @just ./devnet/devnet-down

test: 
  cargo nextest run --workspace

# Formats the world-chain-builder
fmt: fmt-fix fmt-check

fmt-fix:
  cargo +nightly fmt --all

fmt-check:
  cargo +nightly fmt --all -- --check

e2e-test *args='':
    RUST_LOG="info,tests=info" cargo run -p tests-devnet --release -- $@

install *args='':
  cargo install --path crates/world/bin --locked $@