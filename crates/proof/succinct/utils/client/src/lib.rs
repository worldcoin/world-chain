#![cfg_attr(not(test), warn(unused_crate_dependencies))]
//! SP1 guest/host execution utilities for the World Chain OP Succinct Lite programs.

extern crate alloc;

pub mod client;
pub mod precompiles;
pub mod range;
pub mod witness;

pub use range::{OutputRootWitness, RangeProgramError, WorldRangeWitness, run_range_program};
