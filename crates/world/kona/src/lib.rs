//! # World Chain Kona Integration
//!
//! This crate integrates the [Kona](https://github.com/anton-rs/kona) OP Stack consensus node
//! into World Chain so that the **consensus/derivation pipeline** and the **Reth execution engine**
//! run as a **single binary**, communicating via direct Rust function calls rather than the Engine
//! API over HTTP/IPC.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────┐
//! │              world-chain binary                      │
//! │                                                      │
//! │  ┌──────────────────┐   Rust fn calls   ┌──────────┐│
//! │  │   Kona Node      │ ────────────────► │  Reth    ││
//! │  │ (consensus/deriv)│                    │  Engine  ││
//! │  └──────────────────┘                    └──────────┘│
//! └─────────────────────────────────────────────────────┘
//! ```
//!
//! ## Key Components
//!
//! - [`InProcessEngineClient`] — Implements Kona's [`kona_engine::EngineClient`] trait by
//!   dispatching Engine API calls to reth's [`ConsensusEngineHandle`] and reading chain data from
//!   reth's provider. No HTTP, no IPC — just in-process Rust method calls.
//!
//! - [`KonaServiceHandle`] — Manages the lifecycle of the Kona node service, spawning derivation,
//!   engine, network, and RPC actors under the same tokio runtime as reth.
//!
//! ## Status
//!
//! This is a **work-in-progress** integration. Key areas that need further work:
//!
//! - Full compatibility testing between kona's alloy versions and world-chain's
//! - Integration with reth's node builder lifecycle hooks
//! - Configuration bridging (rollup config, genesis, etc.)
//! - End-to-end integration tests
//!
//! See inline `TODO` comments throughout the code.

pub mod engine_client;
pub mod handle;
pub mod config;

pub use engine_client::InProcessEngineClient;
pub use handle::KonaServiceHandle;
pub use config::KonaConfig;
