//! `cargo xtask release` — promote the current measurements into the append-only log.

pub mod log;
pub mod run;
pub mod version;

pub use run::{Args, run};
