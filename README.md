<p align="center">
  <img src="assets/world-chain.png" alt="World Chain">
</p>

<h4 align="center">
    A blockchain designed for humans, built on the <a href="https://stack.optimism.io/">OP Stack</a> and powered by <a href="https://github.com/paradigmxyz/reth"><code>reth</code></a>.
</h4>

<p align="center">
  <a href="https://github.com/worldcoin/world-chain/actions/workflows/rust-ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/worldcoin/world-chain/rust-ci.yml?style=flat&labelColor=1C2C2E&label=ci&color=BEC5C9&logo=GitHub%20Actions&logoColor=BEC5C9" alt="CI"></a>
  <img src="https://img.shields.io/badge/License-MIT-d1d1f6.svg?style=flat&labelColor=1C2C2E&color=BEC5C9&label=license&logoColor=BEC5C9" alt="License">
  <a href="https://world.org"><img src="https://img.shields.io/badge/World-854a15?style=flat&labelColor=1C2C2E&color=BEC5C9&logoColor=BEC5C9" alt="World"></a>
  <a href="https://worldscan.org"><img src="https://img.shields.io/badge/Explorer-854a15?style=flat&labelColor=1C2C2E&color=BEC5C9&logo=ethereum&logoColor=BEC5C9" alt="Explorer"></a>
  <a href="https://deepwiki.com/worldcoin/world-chain"><img src="https://deepwiki.com/badge.svg" alt="Ask DeepWiki"></a>
</p>

<p align="center">
  <a href="#crates">Crates</a> •
  <a href="#proofs">Proofs</a> •
  <a href="#development">Development</a> •
  <a href="#specs">Specs</a> •
  <a href="#security">Security</a> •
  <a href="#license">License</a>
</p>

## Overview

World Chain is a blockchain designed for humans. It prioritizes scalability and accessibility for real users, providing the rails for a frictionless onchain UX. World Chain is built on the [OP Stack](https://stack.optimism.io/) and powered by [reth](https://github.com/paradigmxyz/reth).

## Crates

| Crate | Description |
|-------|-------------|
| [`world-chain-builder`](./crates/builder) | World Chain Payload Builder components |
| [`world-chain-chainspec`](./crates/chainspec) | World Chain specification and genesis configuration. |
| [`world-chain-cli`](./crates/cli) | World Chain CLI |
| [`world-chain-devnet`](./crates/devnet) | Local devnet setup and tooling. |
| [`world-chain-evm`](./crates/evm) | Custom EVM configuration and execution logic. |
| [`world-chain-node`](./crates/node) | World Chain node components builder |
| [`world-chain-p2p`](./crates/p2p) | RLPX Satellite Protocol Components |
| [`world-chain-payload`](./crates/payload) | Payload job lifecycle management, and continuous block building |
| [`world-chain-pbh`](./crates/pbh) | Priority Blockspace for Humans — verified human transaction prioritization. |
| [`world-chain-pool`](./crates/pool) | Transaction pool with custom ordering. |
| [`world-chain-primitives`](./crates/primitives) | Project wide primitives |
| [`world-chain-rpc`](./crates/rpc) | World Chain RPC API Extensions |
| [`world-chain-validator`](./crates/validator) | World Chain Flashblocks Execution Engine |

## Proofs

| Crate | Description |
|-------|-------------|
| [`world-chain-prover`](./proofs/prover) | Shared host-side prover library. |
| [`world-chain-prover-sp1`](./proofs/prover-sp1) | SP1 zkVM prover CLI. |
| [`world-chain-prover-nitro`](./proofs/prover-nitro) | AWS Nitro TEE host prover CLI. |
| [`world-chain-proof-core`](./proofs/core) | Shared primitives for SP1 and Nitro TEE fault-proof backends. |
| [`world-chain-proof-nitro`](./proofs/nitro) | AWS Nitro TEE attestation prover for OP Succinct Lite fault proofs. |
| [`world-chain-proof-protocol`](./proofs/protocol) | Fault-proof protocol definitions and interfaces. |
| [`world-chain-proofs`](./proofs/primitives) | Proof primitives and shared types. |
| [`world-chain-challenger`](./proofs/challenger) | Fault-proof challenger service. |
| [`world-chain-proposer`](./proofs/proposer) | Output root proposer service. |
| [`world-chain-prover-service`](./proofs/prover-service) | Proof generation service. |

## Development

See the [Development Guide](docs/development.md) for building and running World Chain locally.

## Specs

Protocol specifications and design documents are available in the [Specs](specs/overview.md).

## Security

Security issues should be reported privately via [security@toolsforhumanity.com](mailto:security@toolsforhumanity.com). See [`SECURITY.md`](./SECURITY.md) for details.

## License

This project is licensed under the MIT License. See [`LICENSE`](./LICENSE) for details.
