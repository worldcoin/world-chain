//! The typed requirement model.
//!
//! A [`Requirement`] is what an acceptance check declares it needs, and what a
//! manifest commits to. It is one of two kinds:
//!
//! - [`Feature`] — a World Chain feature (flashblocks, block access lists, PBH).
//!   A manifest may commit to an arbitrary set of features.
//! - [`Hardfork`] — a canonical hardfork (e.g. `jovian`, `karst`, `tropo`). A
//!   manifest commits to exactly one hardfork; cumulative ordering means a
//!   requirement is satisfied when the committed hardfork is at or after it.

/// The recognised feature keys. Features form a closed set, so a `requires(...)`
/// naming a feature outside this set is a declaration error.
pub const KNOWN_FEATURE_KEYS: [&str; 3] = ["flashblocks", "block_access_list", "pbh"];

/// A World Chain feature requirement key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Feature(pub &'static str);

/// A canonical hardfork requirement key (lowercase fork name).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hardfork(pub &'static str);

/// A single requirement: either a feature or a hardfork.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Requirement {
    /// A hardfork the network must be at (or past).
    Hardfork(Hardfork),
    /// A feature the network must enable.
    Feature(Feature),
}

impl Requirement {
    /// Construct a hardfork requirement.
    pub const fn hardfork(name: &'static str) -> Self {
        Requirement::Hardfork(Hardfork(name))
    }

    /// Construct a feature requirement.
    pub const fn feature(name: &'static str) -> Self {
        Requirement::Feature(Feature(name))
    }

    /// The requirement key (fork or feature name).
    pub const fn name(&self) -> &'static str {
        match self {
            Requirement::Hardfork(Hardfork(name)) => name,
            Requirement::Feature(Feature(name)) => name,
        }
    }

    /// The requirement kind, `"hardfork"` or `"feature"`.
    pub const fn kind(&self) -> &'static str {
        match self {
            Requirement::Hardfork(_) => "hardfork",
            Requirement::Feature(_) => "feature",
        }
    }

    /// A `kind:name` label for reports and skip reasons.
    pub fn label(&self) -> String {
        format!("{}:{}", self.kind(), self.name())
    }
}

/// Whether `name` is a recognised feature key.
pub fn feature_is_known(name: &str) -> bool {
    KNOWN_FEATURE_KEYS.contains(&name)
}
