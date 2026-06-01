//! World Chain acceptance test harness.
//!
//! A modular, registry-driven suite for asserting network-wide **health**,
//! **spec compatibility**, and **performance** of a World Chain deployment, and
//! for emitting an acceptance report. The same suite runs against a freshly
//! spawned in-process devnet and against a remote alphanet over RPC.
//!
//! # Adding a check
//!
//! Write an `async fn` taking the [`TestCtx`] and annotate it with
//! [`macro@acceptance_test`]. It self-registers at link time — there is no
//! central list to edit.
//!
//! ```ignore
//! use std::sync::Arc;
//! use world_chain_acceptance::{TestCtx, acceptance_test};
//!
//! #[acceptance_test(category = Health)]
//! async fn rpc_is_live(ctx: Arc<TestCtx>) -> eyre::Result<()> {
//!     let _ = ctx.chain_id().await?;
//!     Ok(())
//! }
//! ```
//!
//! # Running
//!
//! Build an [`Env`] with [`Env::builder`] and pass it to [`run`]. The returned
//! [`Report`] renders to JSON, Markdown, and a stdout summary.

// Allow the `acceptance_test` macro's `::world_chain_acceptance::…` paths to
// resolve when the macro is used inside this crate (the bundled checks).
extern crate self as world_chain_acceptance;

pub mod context;
pub mod env;
pub mod registry;
pub mod report;
pub mod runner;

// Bundled checks. Compiled unconditionally so they self-register for any
// consumer of the harness (e.g. the `xtask acceptance` command).
mod checks;

pub use context::{Metric, MetricValue, Skipped, TestCtx};
pub use env::{CloudflareAccess, EngineApi, Env, EnvBuilder, EnvSummary, Thresholds};
pub use registry::{AcceptanceTest, Category, TestFn, TestFuture};
pub use report::{Report, Status, TestResult, Totals};
pub use runner::{
    Execution, InlineExecutor, PanicIsolatedExecutor, RunOptions, TestExecutor, run, run_with,
};

/// The Engine API JWT secret type, re-exported so consumers can construct one
/// without depending on `reth-rpc-layer` directly.
pub use reth_rpc_layer::JwtSecret;

// Re-exported so consumers (and the `acceptance_test` macro) can use the
// manifest types without depending on the manifest crate directly.
pub use world_chain_manifest::{
    self as manifest, Commitments, Feature, Hardfork, NetworkManifest, Requirement,
};

/// The attribute macro that registers an `async fn` as an acceptance check.
pub use world_chain_acceptance_macros::acceptance_test;

// Re-exported so the generated registration code can reference it through this
// crate without consumers needing an `inventory` dependency.
pub use inventory;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_checks_self_register() {
        let tests: Vec<&AcceptanceTest> = inventory::iter::<AcceptanceTest>.into_iter().collect();

        // The bundled spec checks should be collected at link time.
        assert!(
            !tests.is_empty(),
            "expected the bundled spec checks to self-register"
        );
        assert!(
            tests
                .iter()
                .any(|t| t.category == Category::SpecCompatibility),
            "no registered spec-compatibility check"
        );
    }
}
