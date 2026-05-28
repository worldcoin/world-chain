//! # World Chain Kona Integration
//!
//! This crate integrates the [Kona](https://github.com/ethereum-optimism/optimism) OP Stack
//! consensus node into World Chain so that the **consensus/derivation pipeline** runs in the
//! **same binary** as the Reth execution engine.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────┐
//! │              world-chain binary                      │
//! │                                                      │
//! │  ┌──────────────────┐   Engine API     ┌──────────┐ │
//! │  │   Kona Node      │ ───(HTTP/JWT)──► │  Reth     │ │
//! │  │ (consensus/deriv)│   auth RPC :8551 │  Engine   │ │
//! │  └──────────────────┘                   └──────────┘ │
//! └─────────────────────────────────────────────────────┘
//! ```
//!
//! Canonical Optimism kona builds its own [`kona_engine::OpEngineClient`] internally and drives
//! reth over the standard Engine API (HTTP + JWT) against reth's auth RPC endpoint. Both processes
//! share a single tokio runtime, so there is no separate node process to supervise, but the
//! transport between consensus and execution is the regular authenticated Engine API rather than
//! direct in-process Rust calls.
//!
//! ## Key Components
//!
//! - [`KonaServiceHandle`] — Manages the lifecycle of the Kona node service, spawning the
//!   derivation, engine, network, and RPC actors under the same tokio runtime as reth and driving
//!   reth's execution engine over the Engine API.
//!
//! - [`KonaConfig`] — Bridges World Chain's node configuration to kona's
//!   [`kona_node_service::RollupNodeBuilder`] requirements.
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

pub mod config;
pub mod handle;

pub use config::KonaConfig;
pub use handle::KonaServiceHandle;
