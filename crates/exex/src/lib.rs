//! World Chain ExEx playing the role of the OP Proposer.
//!
//! This crate ports the single-chain (pre-interop) slice of
//! [op-proposer](https://github.com/ethereum-optimism/optimism/tree/develop/op-proposer)
//! to a reth [`ExEx`](reth_exex::ExExContext). It periodically reads an L2
//! output root (by default from local node state, optionally from a rollup
//! RPC) and submits it to the L1 `DisputeGameFactory` contract by creating a
//! fault dispute game.
//!
//! Transaction submission goes directly through the alloy contract instance
//! (`factory.create(..).send()`); the wallet-equipped provider's filler stack
//! handles gas estimation (with a 3/2 fallback), nonce management, signing,
//! and fee computation. There is **no custom transaction manager** and
//! **no custom receipt type** — alloy's are used directly.

pub mod config;
pub mod contracts;
pub mod db;
pub mod driver;
pub mod exex;
pub mod local_node;
pub mod metrics;
pub mod provider;
pub mod rpc;
pub mod service;
pub mod source;
pub mod tx_fillers;

pub use config::{ProposerCliArgs, ProposerConfig};
pub use driver::{L2OutputSubmitter, ProposerError};
pub use exex::{install_op_proposer_exex, op_proposer_exex};
pub use provider::{L1Provider, L1ProviderConfig, SignerKind};
pub use service::ProposerService;
pub use source::{Proposal, ProposalSource, SyncStatus};
