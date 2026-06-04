#![cfg_attr(not(test), warn(unused_crate_dependencies))]
//! SP1 guest/host execution utilities for the World Chain OP Succinct Lite programs.

extern crate alloc;

// Retained as dependencies (version pinning / SP1 guest build) but not referenced
// directly here; bind with `as _` to satisfy `unused_crate_dependencies`.
use alloy_eips as _;
use alloy_sol_types as _;
use cfg_if as _;
use revm_precompile as _;
use rkyv as _;
use serde_json as _;
use sha2 as _;

pub mod client;
pub mod precompiles;
pub mod range;
pub mod witness;

pub use range::{OutputRootWitness, RangeProgramError, WorldRangeWitness, run_range_program};
