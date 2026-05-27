//! World Chain ExEx playing the role of the OP Proposer.
//!
//! This crate ports the single-chain (pre-interop) slice of
//! [op-proposer](https://github.com/ethereum-optimism/optimism/tree/develop/op-proposer)
//! to a reth [`ExEx`](reth_exex::ExExContext). It periodically reads an L2
//! output root (by default from local node state, optionally from a rollup
//! RPC) and submits it to the L1 `DisputeGameFactory` contract by creating a
//! fault dispute game.
//!
//! Components (mirroring the Go layout):
//!
//! * [`config`] — CLI / runtime configuration (`flags.go` + `config.go`).
//! * [`source`] — proposal sources (`source/source*.go`).
//! * [`contracts`] — `DisputeGameFactory` & `FaultDisputeGame` bindings.
//! * [`txmgr`] — minimal L1 transaction manager (replaces `op-service/txmgr`).
//! * [`db`] — MDBX-backed persistence for the last submitted proposal.
//! * [`metrics`] — Prometheus metrics (`metrics/metrics.go`).
//! * [`driver`] — main proposer loop (`driver.go`).
//! * [`service`] — service orchestration (`service.go`).
//! * [`rpc`] — `admin_*Proposer` RPC (`proposer/rpc/api.go`).
//! * [`exex`] — reth ExEx entrypoint.

pub mod config;
pub mod contracts;
pub mod db;
pub mod driver;
pub mod exex;
pub mod local_node;
pub mod metrics;
pub mod rpc;
pub mod service;
pub mod source;
pub mod txmgr;

pub use config::{ProposerCliArgs, ProposerConfig};
pub use driver::{L2OutputSubmitter, ProposerError};
pub use exex::{install_op_proposer_exex, op_proposer_exex};
pub use service::ProposerService;
pub use source::{Proposal, ProposalSource, SyncStatus};
