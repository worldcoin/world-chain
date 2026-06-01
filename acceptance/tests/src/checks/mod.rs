//! Bundled acceptance checks.
//!
//! Currently a minimal set of cross-cutting spec-compatibility checks. The
//! harness still supports feature- and hardfork-gated checks via the
//! `requires(...)` attribute argument; add a module per requirement as the
//! suite grows.

mod spec;
