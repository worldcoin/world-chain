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

use std::sync::Arc;

pub use crate::cache::{DEFAULT_WITNESS_CAP, WitnessCache};

/// Thread safe handle to the [`WitnessCache`].
pub type ExecutionWitnessHandle = Arc<WitnessCache>;
