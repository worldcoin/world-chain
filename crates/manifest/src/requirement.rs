//! Canonical requirement keys.
//!
//! A requirement key is the string an acceptance check declares via
//! `requires(...)` and that a manifest commits to. Keys are either a hardfork
//! name (lowercase, read back from the canonical chain spec) or one of the
//! feature keys below.

/// Non-hardfork (feature) requirement keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequirementKey {
    /// Flashblocks are served.
    Flashblocks,
    /// Block access lists (BAL) are produced.
    FlashblocksAccessList,
    /// Priority Blockspace for Humans is enabled.
    Pbh,
}

impl RequirementKey {
    /// The canonical string for this feature key.
    pub const fn as_str(self) -> &'static str {
        match self {
            RequirementKey::Flashblocks => "flashblocks",
            RequirementKey::FlashblocksAccessList => "flashblocks_access_list",
            RequirementKey::Pbh => "pbh",
        }
    }
}

/// All feature requirement keys.
pub const FEATURE_KEYS: [RequirementKey; 3] = [
    RequirementKey::Flashblocks,
    RequirementKey::FlashblocksAccessList,
    RequirementKey::Pbh,
];

/// The feature requirement keys as strings.
pub fn feature_keys() -> impl Iterator<Item = &'static str> {
    FEATURE_KEYS.into_iter().map(RequirementKey::as_str)
}
