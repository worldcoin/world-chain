
<p align="center">
  <img src="assets/world-chain.png" alt="World Chain">
</p>

# World Chain

World Chain is a blockchain designed for humans. Built on the [OP Stack](https://stack.optimism.io/) and powered by [reth](https://github.com/paradigmxyz/reth), World Chain prioritizes scalability and accessibility for real users, providing the rails for a frictionless onchain UX.

## ✨ Key Features

### Priority Blockspace for Humans (PBH)
Verified [World ID](https://world.org/world-id) holders receive priority access to blockspace, ensuring everyday users can transact even during peak network demand. PBH uses zero-knowledge proofs to verify humanity without revealing identity.

**How it works:**
- Top-of-block priority for verified humans
- Monthly transaction quotas with [date-based rate limiting](crates/world/pbh/src/date_marker.rs)
- [Semaphore ZK proofs](crates/world/pbh/src/payload.rs) for privacy-preserving verification
- Reserved blockspace capacity ensures network accessibility

📖 [**PBH Specification**](specs/pbh/overview.md) | [**Architecture**](specs/pbh/architecture.md) | [**Transaction Lifecycle**](docs/pbh_tx_lifecycle.md)

### P2P Flashblocks
A high-speed execution lane that gives builders low-latency settlement for experiences like gaming, social, and real-time commerce. Flashblocks provides sub-second confirmation times for time-sensitive applications.

We use a home baked p2p flashblocks distribution mechanism by adding an additional `rlpx` sub protocol to the exisiting `devp2p` layer.

📦 [**Flashblocks Implementation**](crates/flashblocks)

## 🏗️ Architecture

World Chain extends the OP Stack with custom transaction ordering and validation:

- **Priority Blockspace for Humans**: [Set of crates for World specific functionality](crates/world) 
- **Flashblocks**: [Set of crates that make up flashblocks components](crates/flashblocks)
- **Smart Contracts**: [Solidity contracts](contracts/src) for PBH validation

## 🚀 Getting Started

### Prerequisites
- Rustup
- [Foundry](https://book.getfoundry.sh/) (for smart contracts)
- [Just](https://github.com/casey/just) (task runner)

### Building from Source

```bash
# Clone the repository
git clone https://github.com/worldcoin/world-chain.git
cd world-chain

# Build the node
cargo build --release

# Run tests
cargo test
```

### Running a Local Devnet

Use [Kurtosis](https://www.kurtosis.com/) for local development and testing:

```bash
just devnet-up
```

See [devnet documentation](devnet/) for configuration options and stress testing.

### Downloading Snapshots

`reth` snapshots are regularly updated and can be downloaded and extracted with the following commands:

```bash
BUCKET="world-chain-snapshots" # use world-chain-testnet-snapshots for sepolia
FILE_NAME="reth_archive.tar.lz4" # reth_full.tar.lz4 is available on mainnet only
OUT_DIR="./" # path to where you would like reth dir to end up
VID="$(aws s3api head-object --bucket "$BUCKET" --key "$FILE_NAME" --region eu-central-2 --query 'VersionId' --output text)"
aws s3api get-object --bucket "$BUCKET" --key "$FILE_NAME" --version-id "$VID" --region eu-central-2 --no-cli-pager /dev/stdout | lz4 -d | tar -C "$OUT_DIR" -x
```

## 📚 Documentation

- [**Specifications**](specs/) - Detailed technical specifications and architecture
- [**PBH Overview**](specs/pbh/overview.md) - Priority Blockspace for Humans concept
- [**PBH Transaction Lifecycle**](docs/pbh_tx_lifecycle.md) - Complete walkthrough of PBH transactions
- [**Validation Rules**](specs/pbh/validation.md) - Transaction validation requirements

## 🧰 Codebase Structure

```
world-chain/
├── crates/
│   ├── world/           # Core World Chain node implementation
│   ├── flashblocks/     # Components for flashblocks construction, propagation, and execution
│   └── toolkit/         # CLI utilities
├── contracts/           # Solidity smart contracts (Foundry)
├── specs/               # Technical specifications (mdBook)
├── docs/                # Additional documentation
├── devnet/              # Local development environment (Kurtosis)
└── snapshotter/         # Database snapshot script
```

## 🤝 Contributing

Contributions are welcome! Please see our contributing guidelines and code of conduct.

## 📄 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## 🔗 Links

- [World Chain Explorer](https://worldscan.org)
- [World ID Documentation](https://docs.world.org)
- [OP Stack](https://stack.optimism.io/)
- [Reth](https://github.com/paradigmxyz/reth)
