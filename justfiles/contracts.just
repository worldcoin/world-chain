# Foundry / forge recipes. Most delegate to pkg/contracts/Justfile.

[group('contracts')]
setup-contracts:
    @just ./pkg/contracts/setup-contracts

# Build the pinned Optimism implementation contracts in the isolated opstack/ sub-project.
[group('contracts')]
build-opstack:
    @just ./pkg/contracts/build-opstack

alias bc := build-contracts

[group('contracts')]
build-contracts *args='':
    @just ./pkg/contracts/build-contracts $@

alias tc := test-contracts

[group('contracts')]
test-contracts *args='':
    @just ./pkg/contracts/test-contracts $@

[group('contracts')]
contracts-fmt:
    @just ./pkg/contracts/fmt

[group('contracts')]
deploy-contracts:
    @just ./pkg/contracts/deploy-contracts
