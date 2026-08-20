//! `cargo xtask measure` — rebuild what the proof system commits to and record it.

pub mod lock;
pub mod run;

pub use run::{Args, run};
