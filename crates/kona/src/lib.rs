//! # World Chain Kona Integration
//!
//! This crate runs the [Kona](https://github.com/ethereum-optimism/optimism) OP Stack consensus
//! node **in-process** alongside the reth execution engine, in the same binary.
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────┐
//! │                     world-chain binary                        │
//! │                                                                │
//! │  ┌──────────────────┐  in-process Rust calls  ┌─────────────┐ │
//! │  │   Kona actors    │ ──────────────────────► │ reth Engine │ │
//! │  │ (consensus/deriv)│  ConsensusEngineHandle   │  (EL tree)  │ │
//! │  └──────────────────┘  + PayloadStore          └─────────────┘ │
//! └──────────────────────────────────────────────────────────────┘
//! ```
//!
//! Unlike canonical kona — which drives reth over the authenticated Engine API (HTTP + JWT) — the
//! consensus hot path (`fork_choice_updated`, `new_payload`, `get_payload`) is dispatched directly
//! to reth's [`reth_engine_primitives::ConsensusEngineHandle`] and
//! [`reth_payload_builder::PayloadStore`] via [`WorldChainKonaEngineClient`]. There is no separate node
//! process and no network transport on that path.
//!
//! ## Key Components
//!
//! - [`WorldChainKonaEngineClient`] — Implements kona's [`kona_engine::EngineClient`] trait by
//!   dispatching Engine API calls in-process to reth.
//! - [`KonaService`] — Manually assembles the kona actor graph (engine, derivation, network, L1
//!   watcher, optional sequencer, optional RPC) around the in-process engine client.
//! - [`KonaServiceHandle`] — Owns the spawned service task and its cancellation token.
//! - [`KonaConfig`] — Bridges World Chain's node configuration to the kona service inputs.

pub mod client;
pub mod config;
pub mod service;

pub use client::WorldChainKonaEngineClient;
pub use config::KonaConfig;
pub use service::{KonaService, KonaServiceHandle};
