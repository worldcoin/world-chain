//! Guest/client utilities for the World Chain OP Succinct Lite programs.
//!
//! These types intentionally mirror OP Succinct/Base public values. The World-specific layer is the
//! fork schedule and spec selection used while constructing the range proof boot values.

pub mod boot;
pub mod range;
pub mod types;

pub use range::{RangeProgramError, WorldRangeWitness, run_range_program};
