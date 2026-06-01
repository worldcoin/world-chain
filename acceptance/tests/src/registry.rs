//! The acceptance-test descriptor and the link-time registry it lives in.

use std::{fmt, future::Future, pin::Pin, sync::Arc};

use serde::{Serialize, Serializer};
use world_chain_manifest::Requirement;

use crate::context::TestCtx;

/// Future returned by a registered acceptance check.
pub type TestFuture = Pin<Box<dyn Future<Output = eyre::Result<()>> + Send>>;

/// Function pointer the harness invokes for each registered check.
///
/// The [`acceptance_test`](crate::acceptance_test) attribute generates a
/// non-capturing closure of this shape that boxes the annotated `async fn`.
pub type TestFn = fn(Arc<TestCtx>) -> TestFuture;

/// Test classification. Drives report grouping and `--category` filtering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    /// Network is live and making progress (RPC up, blocks/safe head advancing).
    Health,
    /// The deployment matches the World Chain specification (chain id,
    /// advertised capabilities, hardfork activation).
    SpecCompatibility,
    /// The network meets performance budgets (block time, latency, throughput).
    Performance,
}

impl Category {
    /// Stable lowercase identifier used by the `--category` CLI filter.
    pub const fn as_str(self) -> &'static str {
        match self {
            Category::Health => "health",
            Category::SpecCompatibility => "spec",
            Category::Performance => "performance",
        }
    }

    /// Parse a `--category` filter token. Accepts a few friendly aliases.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "health" => Some(Category::Health),
            "spec" | "spec_compatibility" | "spec-compatibility" => {
                Some(Category::SpecCompatibility)
            }
            "performance" | "perf" => Some(Category::Performance),
            _ => None,
        }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for Category {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// A single registered acceptance check.
///
/// Instances are submitted by the [`acceptance_test`](crate::acceptance_test)
/// attribute and collected via [`inventory`]. Iterate the full set with
/// [`inventory::iter::<AcceptanceTest>`](inventory::iter).
pub struct AcceptanceTest {
    /// Display name (defaults to the de-snake-cased function name).
    pub name: &'static str,
    /// Classification used for grouping and filtering.
    pub category: Category,
    /// Typed requirements (features and/or a hardfork) the network must commit
    /// to for this check to run. A check whose requirements the manifest does
    /// not commit to is reported as skipped rather than failed, so the same
    /// suite runs against any deployment that commits to a superset.
    pub requires: &'static [Requirement],
    /// The check body.
    pub run: TestFn,
}

inventory::collect!(AcceptanceTest);
