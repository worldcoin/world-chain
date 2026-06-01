//! Bundled acceptance checks.
//!
//! Each submodule registers checks for one category via the
//! [`acceptance_test`](crate::acceptance_test) attribute. New checks are added
//! by writing another annotated function here (or in any crate that links the
//! harness) — nothing else needs to change.

mod health;
mod performance;
mod spec;
