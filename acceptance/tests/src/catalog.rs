//! Link-time acceptance-test catalog.

use std::{future::Future, pin::Pin, str::FromStr, sync::Arc};

use world_chain_chainspec::{Feature, ManifestCommitment, WorldChainHardfork, hardfork_key};

use crate::context::TestCtx;

/// Future returned by a registered acceptance test.
pub type TestFuture = Pin<Box<dyn Future<Output = eyre::Result<()>> + Send>>;

/// Function pointer the harness invokes for each registered test.
pub type TestFn = fn(Arc<TestCtx>) -> TestFuture;

/// Source location for a registered acceptance test.
#[derive(Clone, Copy, Debug)]
pub struct Location {
    pub file: &'static str,
    pub line: u32,
}

/// Declarative applicability metadata attached to a test by the
/// [`acceptance_test`](crate::acceptance_test) macro.
///
/// The hardfork/feature names are stored verbatim (as written in the attribute)
/// and resolved against the typed enums at selection time. This is the central
/// improvement over a purely imperative `if !fork_active { return }` model: the
/// runner can pre-select and report coverage by fork/feature without executing
/// the test body.
#[derive(Clone, Copy, Debug)]
pub struct Requirements {
    /// Minimum World Chain hardfork the test needs (a stable hardfork key, e.g.
    /// `"tropo"`). `None` means the test applies to every committed fork.
    pub min_hardfork: Option<&'static str>,
    /// Features the test needs (stable feature keys, e.g. `"flashblocks"`). The
    /// manifest must commit to all of them for the test to run.
    pub features: &'static [&'static str],
    /// When `true`, the test opts out of intra-cell parallelism.
    pub serial: bool,
}

impl Requirements {
    /// An empty requirement set: applies to every committed manifest.
    pub const NONE: Self = Self {
        min_hardfork: None,
        features: &[],
        serial: false,
    };

    /// Decide whether a test with these requirements should run against a
    /// network committed to `commitment`.
    pub fn applicability(&self, commitment: &ManifestCommitment) -> Applicability {
        if let Some(name) = self.min_hardfork {
            let Ok(fork) = WorldChainHardfork::from_str(name) else {
                return Applicability::Invalid(format!("unknown required hardfork `{name}`"));
            };
            if !commitment.includes_hardfork(fork) {
                return Applicability::Skip(format!(
                    "requires hardfork `{name}`; manifest commits to `{}`",
                    hardfork_key(commitment.hardfork())
                ));
            }
        }

        for name in self.features {
            let Ok(feature) = Feature::from_str(name) else {
                return Applicability::Invalid(format!("unknown required feature `{name}`"));
            };
            if !commitment.includes_feature(feature) {
                return Applicability::Skip(format!(
                    "manifest does not commit to feature `{name}`"
                ));
            }
        }

        Applicability::Applicable
    }
}

/// The outcome of evaluating a test's [`Requirements`] against a manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applicability {
    /// The test applies and should be executed.
    Applicable,
    /// The test does not apply; skip it with this reason.
    Skip(String),
    /// The test's requirements are malformed (e.g. an unknown fork name). This
    /// is a programming error and surfaces as a failure, not a skip.
    Invalid(String),
}

/// A single registered acceptance test.
///
/// The [`acceptance_test`](crate::acceptance_test) attribute submits one of
/// these descriptors via `inventory`. There is intentionally no external gate
/// or category file to keep in sync: the catalog is derived from the test code.
#[derive(Debug)]
pub struct AcceptanceTest {
    /// Stable test name (defaults to the function name).
    pub name: &'static str,
    /// Rust module path, used like Optimism's Go package path.
    pub package: &'static str,
    /// Source location for reports and flaky-test artifacts.
    pub location: Location,
    /// Reason this test is allowed to downgrade failures to skips.
    pub flaky: Option<&'static str>,
    /// Declarative hardfork/feature applicability for selection and gating.
    pub requirements: Requirements,
    /// The test body.
    pub run: TestFn,
}

/// Collected acceptance tests.
#[derive(Debug)]
pub struct Catalog {
    tests: Vec<&'static AcceptanceTest>,
}

impl Catalog {
    /// Collect every linked [`AcceptanceTest`] submitted through `inventory`.
    pub fn collect() -> Self {
        let mut tests: Vec<_> = inventory::iter::<AcceptanceTest>.into_iter().collect();
        tests.sort_by(|a, b| {
            a.package
                .cmp(b.package)
                .then_with(|| a.name.cmp(b.name))
                .then_with(|| a.location.file.cmp(b.location.file))
                .then_with(|| a.location.line.cmp(&b.location.line))
        });
        Self { tests }
    }

    /// Iterate all registered tests in stable order.
    pub fn iter(&self) -> impl Iterator<Item = &'static AcceptanceTest> + '_ {
        self.tests.iter().copied()
    }
}

impl Default for Catalog {
    fn default() -> Self {
        Self::collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commitment(hardfork: WorldChainHardfork, features: &[Feature]) -> ManifestCommitment {
        ManifestCommitment::new(hardfork, features.iter().copied())
    }

    #[test]
    fn empty_requirements_always_apply() {
        let req = Requirements::NONE;
        let c = commitment(WorldChainHardfork::Jovian, &[]);
        assert_eq!(req.applicability(&c), Applicability::Applicable);
    }

    #[test]
    fn hardfork_gate_skips_earlier_fork() {
        let req = Requirements {
            min_hardfork: Some("tropo"),
            features: &[],
            serial: false,
        };
        let c = commitment(WorldChainHardfork::Jovian, &[]);
        assert!(matches!(req.applicability(&c), Applicability::Skip(_)));

        let c = commitment(WorldChainHardfork::Tropo, &[]);
        assert_eq!(req.applicability(&c), Applicability::Applicable);
    }

    #[test]
    fn feature_gate_requires_all_features() {
        let req = Requirements {
            min_hardfork: None,
            features: &["flashblocks"],
            serial: false,
        };
        let c = commitment(WorldChainHardfork::Jovian, &[]);
        assert!(matches!(req.applicability(&c), Applicability::Skip(_)));

        let c = commitment(WorldChainHardfork::Jovian, &[Feature::Flashblocks]);
        assert_eq!(req.applicability(&c), Applicability::Applicable);
    }

    #[test]
    fn unknown_requirement_is_invalid() {
        let req = Requirements {
            min_hardfork: Some("karst"),
            features: &[],
            serial: false,
        };
        let c = commitment(WorldChainHardfork::Strato, &[]);
        assert!(matches!(req.applicability(&c), Applicability::Invalid(_)));
    }
}
