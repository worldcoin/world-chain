<h1 align="center">
  <img src="assets/world-chain.png" alt="World Chain" width="50%">
</h1>

<h4 align="center">
    A blockchain designed for humans, built on the <a href="https://stack.optimism.io/">OP Stack</a> and powered by <a href="https://github.com/paradigmxyz/reth"><code>reth</code></a>.
</h4>

<p align="center">
  <a href="https://github.com/worldcoin/world-chain/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/worldcoin/world-chain/ci.yml?style=flat&labelColor=1C2C2E&label=ci&color=BEC5C9&logo=GitHub%20Actions&logoColor=BEC5C9" alt="CI"></a>
  <img src="https://img.shields.io/badge/License-MIT-d1d1f6.svg?style=flat&labelColor=1C2C2E&color=BEC5C9&logo=googledocs&label=license&logoColor=BEC5C9" alt="License">
  <a href="https://world.org"><img src="https://img.shields.io/badge/World-854a15?style=flat&labelColor=1C2C2E&color=BEC5C9&logo=data:image/svg+xml;base64,&logoColor=BEC5C9" alt="World"></a>
  <a href="https://worldscan.org"><img src="https://img.shields.io/badge/Explorer-854a15?style=flat&labelColor=1C2C2E&color=BEC5C9&logo=ethereum&logoColor=BEC5C9" alt="Explorer"></a>
</p>

<p align="center">
  <a href="#overview">Overview</a> •
  <a href="#crates">Crates</a> •
  <a href="#development">Development</a> •
  <a href="#specs">Specs</a> •
  <a href="#security">Security</a> •
  <a href="#license">License</a>
</p>

## Overview

World Chain is a blockchain designed for humans. It prioritizes scalability and accessibility for real users, providing the rails for a frictionless onchain UX. World Chain is built on the [OP Stack](https://stack.optimism.io/) and powered by [reth](https://github.com/paradigmxyz/reth).

| Component | Role |
|-----------|------|
| [`world-chain-builder`](./crates/builder) | Custom block builder with priority blockspace for humans (PBH). |
| [`world-chain-node`](./crates/node) | World Chain execution node built on reth. |
| [`world-chain-evm`](./crates/evm) | Custom EVM configuration and execution logic. |
| [`world-chain-pool`](./crates/pool) | Transaction pool with PBH-aware ordering. |
| [`world-chain-pbh`](./crates/pbh) | Priority Blockspace for Humans — verified human transaction prioritization. |
| [`world-chain-validator`](./crates/validator) | Transaction validation with World ID proof verification. |
| [`world-chain-rpc`](./crates/rpc) | Custom RPC extensions for World Chain. |

## Crates

| Crate | Description |
|-------|-------------|
| [`world-chain-chainspec`](./crates/chainspec) | Chain specification and genesis configuration. |
| [`world-chain-cli`](./crates/cli) | CLI tooling for operating World Chain nodes. |
| [`world-chain-devnet`](./crates/devnet) | Local devnet setup and tooling. |
| [`world-chain-p2p`](./crates/p2p) | Peer-to-peer networking layer. |
| [`world-chain-payload`](./crates/payload) | Payload building and attributes. |
| [`world-chain-primitives`](./crates/primitives) | Shared types and primitives. |
| [`world-chain-state`](./crates/state) | State management and storage. |

## Development

See the [Development Guide](docs/development.md) for building and running World Chain locally.

## Specs

Protocol specifications and design documents are available in the [Specs](specs/overview.md).

## Security

Security issues should be reported privately via [security@toolsforhumanity.com](mailto:security@toolsforhumanity.com). See [`SECURITY.md`](./SECURITY.md) for details.

## License

This project is licensed under the MIT License. See [`LICENSE`](./LICENSE) for details.
