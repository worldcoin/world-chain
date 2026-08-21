# Cargo, rustfmt, clippy and the container image.

[group('build')]
build:
    docker buildx build \
        --build-arg VERGEN_GIT_SHA="$(git rev-parse HEAD)" \
        -t world-chain:latest .

[group('build')]
build-world-chain-bin:
    cargo build -p world-chain

[group('build')]
install *args='':
    cargo install --path bin/world-chain --locked $@

# Generate CLI reference docs for the mdbook
[group('build')]
docs:
    cargo xtask docs

[group('test')]
test *args='':
    RUST_LOG="info" cargo nextest run --workspace $@

# Test with flashblocks debug tracing
[group('test')]
test-dev *args='':
    RUST_LOG="info,flashblocks=debug,world_chain=info" cargo nextest run --workspace $@

# Test with verbose flashblocks tracing (all subsystems at trace level)
[group('test')]
test-verbose *args='':
    RUST_LOG="info,flashblocks=trace,world_chain=trace,bal_executor=trace,payload_builder=trace,engine::tree=trace" cargo nextest run --workspace $@

[group('lint')]
clippy:
    cargo +nightly-2026-07-01 clippy --workspace --all-targets --all-features

[group('lint')]
fmt: fmt-fix fmt-check contracts-fmt

[group('lint')]
fmt-fix:
    cargo +nightly-2026-07-01 fmt --all

[group('lint')]
fmt-check:
    cargo +nightly-2026-07-01 fmt --all -- --check
