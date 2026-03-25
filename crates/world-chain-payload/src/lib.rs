#![cfg_attr(not(test), warn(unused_crate_dependencies))]

//! Payload job generation for flashblocks.
//!
//! This crate contains:
//!
//! - [`generator`] - Payload job generator that creates new payload build jobs
//! - [`job`] - Flashblocks payload job implementation with incremental building
//! - [`metrics`] - Payload builder metrics tracking
//! - [`builder`] - World Chain payload builder (non-flashblocks path)
//! - [`context`] - World Chain payload builder context and PBH transaction handling

pub mod builder;
pub mod context;
pub mod generator;
pub mod job;
pub mod metrics;

pub use generator::*;
pub use job::*;
pub use metrics::*;
