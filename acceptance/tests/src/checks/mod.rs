//! Bundled acceptance checks.
//!
//! Cross-cutting checks live in `health`, `performance`, and `spec`. Feature-
//! and hardfork-specific checks live in a folder per requirement, so the suite
//! for a given feature or fork is self-contained and easy to extend:
//!
//! - `flashblocks/`        — `requires(feature = flashblocks)`
//! - `block_access_list/`  — `requires(feature = block_access_list)`
//! - `jovian/`             — `requires(hardfork = jovian)`
//! - `karst/`              — `requires(hardfork = karst)`
//! - `tropo/`              — `requires(hardfork = tropo)`
//!
//! Hardfork gating is cumulative: a folder runs when the manifest's committed
//! hardfork is at or after it.

// Cross-cutting (ungated) checks.
mod health;
mod performance;
mod spec;

// Feature folders.
mod block_access_list;
mod flashblocks;

// Hardfork folders.
mod jovian;
mod karst;
mod tropo;
