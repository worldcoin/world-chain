set positional-arguments

# default recipe to display help information
default:
    @just --list

# Spawns the devnet
# Optional arguments: profile=<dev|release|maxperf> (default: dev)
devnet-up profile="dev": (deploy-devnet profile) deploy-contracts

deploy-devnet profile="dev": (build profile)
  @just ./devnet/devnet-up

# Build with configurable profile
# Usage: just build           (uses dev profile - fastest build, default)
#        just build dev        (fastest build, slowest runtime - default)
#        just build release    (balanced - fast build, good performance)
#        just build maxperf    (slowest build, best runtime)
build profile="dev":
  docker buildx build --build-arg BUILD_PROFILE={{profile}} -t world-chain:latest .

deploy-contracts:
  @just ./contracts/deploy-contracts

# Stops the devnet **This will prune all docker containers**
devnet-down:
  @just ./devnet/devnet-down

test: 
  cargo nextest run --workspace

# Formats the whole workspace
fmt: devnet-fmt contracts-fmt fmt-fix fmt-check

devnet-fmt:
  @just ./devnet/fmt

contracts-fmt:
  @just ./contracts/fmt

fmt-fix:
  cargo +nightly fmt --all

fmt-check:
  cargo +nightly fmt --all -- --check

e2e-test *args='':
  RUST_LOG="info,tests=info" cargo run -p tests-devnet --release -- $@

install *args='':
  cargo install --path crates/world/bin --locked $@

# Run stress tests (ensures maxperf profile is used for optimal performance)
# Usage: just stress-test stress           # Run normal stress test
#        just stress-test stress-precompile # Run precompile stress test
#        just stress-test report           # Generate report
stress-test *args='':
  @echo "Note: Stress tests should be run against a maxperf build for accurate results"
  @echo "If devnet is not running with maxperf, rebuild with: just devnet-up maxperf"
  @just ./devnet/stress-test $@

# Build and deploy devnet with maxperf profile for stress testing
stress-devnet-up: (devnet-up "maxperf")