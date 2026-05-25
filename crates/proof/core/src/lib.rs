//! Shared proof primitives for World Chain fault-proof backends.
//!
//! Types in this crate are used by both the SP1/Succinct and the AWS Nitro TEE backends.
//! Neither backend-specific infrastructure (SP1 zkVM, vsock, NSM) nor execution-pipeline
//! helpers (derivation drivers, EVM factories) belong here.

extern crate alloc;

pub mod artifacts;
pub mod boot;
pub mod oracle;
pub mod range;
pub mod types;
pub mod witness;

pub use boot::{RollupConfigHashError, hash_rollup_config, hash_world_rollup_config_generic};
pub use oracle::BlobStore;
pub use witness::preimage_store::PreimageStore;
