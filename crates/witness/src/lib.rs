#![cfg_attr(not(test), warn(unused_crate_dependencies))]

//! Live pre-image witness oracle for the World Chain node.
//!
//! When enabled, the node captures each block's execution witness during live `newPayload` import
//! and stores it in a bounded in-memory [`WitnessCache`]. A range of these witnesses is served
//! over RPC (`debug_collectRangeWitness`) so the proof system can pre-seed its Kona host key/value
//! store instead of re-deriving the L2-state portion of a range witness from scratch.
//!
//! See `docs/proof/live-preimage-witness-oracle.md` for the full design.

mod cache;
mod types;

pub use cache::WitnessCache;
pub use types::{BlockWitness, RangeWitness};

use std::sync::Arc;

/// Shared handle to the [`WitnessCache`], cloned between the capture path and the RPC handler.
pub type WitnessCacheHandle = Arc<WitnessCache>;
