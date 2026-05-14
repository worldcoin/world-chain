# Local World Mainnet Prover

This workflow is for local, read-only mainnet proving tests. It does not deploy contracts, create
fault dispute games, submit proofs, or require a funded L1 signer.

## Prerequisites

- `WORLD_CHAIN_MAINNET_L2_RPC_URL`: World Chain mainnet execution RPC with historical `eth_call`,
  finalized block tag support, and either `eth_getProof` for the target block or
  `optimism_outputAtBlock`. The World-hosted public RPC is enough for headers and calls, but rejects
  `optimism_outputAtBlock`; the Just recipes default to the public Alchemy World RPC.
- `WORLD_CHAIN_MAINNET_L1_RPC_URL`: Ethereum L1 execution RPC with historical block/header access.
- `WORLD_CHAIN_MAINNET_L1_BEACON_RPC_URL`: Ethereum beacon API endpoint for EIP-4844 blob sidecars.
  For reliable proving, use infrastructure that also exposes the L2 debug/preimage data required by
  Kona; some public RPCs will seed output roots but still time out during full witness generation.
- Either `WORLD_CHAIN_MAINNET_ROLLUP_CONFIG` pointing at the mainnet rollup config JSON, or
  `WORLD_CHAIN_MAINNET_ROLLUP_CONFIG_HASH` with the already computed OP Succinct rollup config hash.
  The Just recipes default to the public Alchemy RPC and the current repo-local mainnet proof
  schedule hash for smoke tests.
- `cargo prove` and Docker for deterministic ELF builds. Install the SP1 toolchain with
  `curl -L https://sp1up.succinct.xyz | bash`, then run `sp1up -v 6.1.0` and verify with
  `cargo prove --version`.
- `protobuf` for the local SP1 SDK prover (`brew install protobuf` on macOS).

## Build SP1 ELFs

```sh
just build-proof-elfs
```

The range ELF is written to `crates/proof/succinct/elf/world-chain-range-ethereum`.
The aggregation ELF is written to `crates/proof/succinct/elf/world-chain-aggregation`.
For a faster local smoke test without Docker, use `just build-proof-range-elf-local`.

## Inspect Verifying Keys

```sh
just proof-vkeys
```

Record the range vkey, aggregation vkey, and rollup config hash before running longer tests.

## Build A Mainnet Witness

```sh
just witness-mainnet-block 12345678
```

This fetches block `N - 1` and `N`, seeds the OP output roots, runs the Kona derivation/execution
client against a preimage server, and writes an rkyv-encoded SP1 witness under `proofs/`. Metadata
is written beside it as JSON. For a not-yet-finalized test block, run the CLI directly and add
`--allow-unfinalized`.

## Prove A Mainnet Block Locally

```sh
just prove-and-verify-mainnet-block 12345678 core
```

This creates a proof, verifies it with the local SP1 verifier, then writes the proof and metadata
artifacts. `core` is the fastest local proof mode. Use `compressed`, `plonk`, or `groth16` only
after the core proof path works for the target block.

If proving reports missing `protoc`, install protobuf with `brew install protobuf`. If it reports a
missing range ELF, run `just build-proof-range-elf`.
