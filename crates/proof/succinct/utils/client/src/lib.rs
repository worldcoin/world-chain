//! Guest/client utilities for the World Chain OP Succinct Lite programs.
//!
//! These types intentionally mirror OP Succinct/Base public values. The World-specific layer is the
//! fork schedule and spec selection used while constructing the range proof boot values.

extern crate alloc;

pub mod boot;
pub mod client;
pub mod oracle;
pub mod precompiles;
pub mod range;
pub mod types;
pub mod witness;

pub use oracle::BlobStore;
pub use range::{
    RangeProgramError, WorldRangeHardfork, WorldRangeHardforkConfig, WorldRangeProofClaim,
    WorldRangeProofInput, WorldRangeProofPublicValues, WorldRangeSpecId, WorldRangeWitness,
    run_range_program,
};
