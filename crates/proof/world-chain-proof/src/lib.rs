//! World Chain fault-proof library.
//!
//! # Feature flags
//!
//! | Flag | Backend | Platform |
//! |------|---------|----------|
//! | `sp1` | SP1/Succinct zkVM | any |
//! | `aws_nitro` | AWS Nitro TEE | Linux only (AF_VSOCK) |
//!
//! Shared primitives (boot values, range types, witness data, oracle, artifacts) are always
//! available without enabling any feature flag.

pub use world_chain_proof_core::*;

#[cfg(feature = "sp1")]
pub mod sp1 {
    pub use world_chain_proof_succinct_client_utils::*;
    pub use world_chain_proof_succinct_utils::*;
}

#[cfg(feature = "aws_nitro")]
pub mod aws_nitro {
    pub use world_chain_proof_nitro::*;
}
