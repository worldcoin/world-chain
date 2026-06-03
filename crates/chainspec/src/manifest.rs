//! World Chain acceptance manifest.
//!
//! The schema is intentionally small and exact:
//!
//! ```toml
//! name = "alphanet"                         # required string
//! hardfork = "jovian"                       # required WorldChainHardfork
//! features = ["flashblocks", "pbh"]         # optional unique Feature list
//!
//! [chain]                                   # required
//! spec = "dev"                              # exactly one of spec/genesis
//! # genesis = "genesis.json"
//! chain_id = 2151908                        # optional chain id override
//! ```
//!
//! `hardfork` is always a [`WorldChainHardfork`]. `features` is a closed,
//! typed list of [`Feature`] values. The acceptance harness uses this as target
//! network configuration, while the test catalog itself is derived from code.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use crate::{WorldChainHardfork, WorldChainSpec};
use alloy_chains::Chain;
use alloy_genesis::Genesis;
use eyre::eyre::{Result, WrapErr, bail, eyre};
use serde::{Deserialize, Serialize};

mod feature;

pub use feature::Feature;
/// Canonical World Chain hardfork order used by manifest selection.
pub const WORLD_CHAIN_HARDFORKS: [WorldChainHardfork; 11] = [
    WorldChainHardfork::Bedrock,
    WorldChainHardfork::Regolith,
    WorldChainHardfork::Canyon,
    WorldChainHardfork::Ecotone,
    WorldChainHardfork::Fjord,
    WorldChainHardfork::Granite,
    WorldChainHardfork::Holocene,
    WorldChainHardfork::Isthmus,
    WorldChainHardfork::Jovian,
    WorldChainHardfork::Tropo,
    WorldChainHardfork::Strato,
];

/// Stable manifest key for a hardfork.
pub fn hardfork_key(fork: WorldChainHardfork) -> &'static str {
    match fork {
        WorldChainHardfork::Bedrock => "bedrock",
        WorldChainHardfork::Regolith => "regolith",
        WorldChainHardfork::Canyon => "canyon",
        WorldChainHardfork::Ecotone => "ecotone",
        WorldChainHardfork::Fjord => "fjord",
        WorldChainHardfork::Granite => "granite",
        WorldChainHardfork::Holocene => "holocene",
        WorldChainHardfork::Isthmus => "isthmus",
        WorldChainHardfork::Jovian => "jovian",
        WorldChainHardfork::Tropo => "tropo",
        WorldChainHardfork::Strato => "strato",
    }
}

/// A committed network manifest.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkManifest {
    /// Human-readable network name (e.g. `alphanet`).
    pub name: String,
    /// The single World Chain hardfork this network commits to.
    #[serde(with = "hardfork_serde")]
    pub hardfork: WorldChainHardfork,
    /// Feature slices this network commits to.
    #[serde(default)]
    pub features: Vec<Feature>,
    /// Canonical chain configuration this network commits to.
    pub chain: ChainConfig,
    /// Directory the manifest was loaded from, used to resolve a relative
    /// `chain.genesis` path. Not part of the serialized document.
    #[serde(skip)]
    base_dir: Option<PathBuf>,
}

/// Canonical chain configuration: exactly one of `spec` or `genesis`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainConfig {
    /// A predefined OP/World chain spec name (e.g. `dev`, `worldchain`,
    /// `worldchain-sepolia`), resolved via [`WorldChainSpec::parse_chain`].
    #[serde(default)]
    pub spec: Option<String>,
    /// Path to a canonical [`Genesis`] JSON file (relative to the manifest).
    #[serde(default)]
    pub genesis: Option<PathBuf>,
    /// Optional chain-id override applied to the derived spec.
    #[serde(default)]
    pub chain_id: Option<u64>,
}

/// The manifest fields that describe acceptance-test applicability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestCommitment {
    hardfork: WorldChainHardfork,
    features: BTreeSet<Feature>,
}

impl ManifestCommitment {
    pub fn new(hardfork: WorldChainHardfork, features: impl IntoIterator<Item = Feature>) -> Self {
        Self {
            hardfork,
            features: features.into_iter().collect(),
        }
    }

    pub const fn hardfork(&self) -> WorldChainHardfork {
        self.hardfork
    }

    pub fn features(&self) -> &BTreeSet<Feature> {
        &self.features
    }

    pub fn includes_feature(&self, feature: Feature) -> bool {
        self.features.contains(&feature)
    }

    pub fn includes_hardfork(&self, fork: WorldChainHardfork) -> bool {
        fork.idx() <= self.hardfork.idx()
    }

    pub fn committed_keys(&self) -> Vec<String> {
        let mut keys = Vec::with_capacity(self.features.len() + 1);
        keys.push(format!("hardfork:{}", hardfork_key(self.hardfork)));
        keys.extend(
            self.features
                .iter()
                .map(|feature| format!("feature:{feature}")),
        );
        keys
    }
}

impl NetworkManifest {
    /// Load and validate a manifest from a TOML file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let contents = std::fs::read_to_string(path)
            .wrap_err_with(|| format!("failed to read manifest {}", path.display()))?;
        let mut manifest = Self::from_toml_str(&contents)
            .wrap_err_with(|| format!("invalid manifest {}", path.display()))?;
        manifest.base_dir = path.parent().map(Path::to_path_buf);
        manifest
            .into_chain_spec()
            .wrap_err_with(|| format!("manifest {} has an invalid chain source", path.display()))?;
        Ok(manifest)
    }

    /// Parse and validate a manifest from a TOML string.
    pub fn from_toml_str(contents: &str) -> Result<Self> {
        let manifest: Self = toml::from_str(contents).wrap_err("failed to parse manifest TOML")?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Whether `path` looks like a manifest file (a `.toml` path).
    pub fn is_manifest_path(path: &str) -> bool {
        Path::new(path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
    }

    /// Build the canonical [`WorldChainSpec`] this manifest commits to.
    pub fn into_chain_spec(&self) -> Result<WorldChainSpec> {
        let mut spec = match (&self.chain.spec, &self.chain.genesis) {
            (Some(name), None) => {
                let spec = WorldChainSpec::parse_chain(name)
                    .ok_or_else(|| eyre!("unknown chain spec `{name}`"))?;
                (*spec).clone()
            }
            (None, Some(genesis)) => {
                let path = self.resolve(genesis);
                let contents = std::fs::read_to_string(&path)
                    .wrap_err_with(|| format!("failed to read genesis {}", path.display()))?;
                let genesis: Genesis = serde_json::from_str(&contents)
                    .wrap_err_with(|| format!("failed to parse genesis {}", path.display()))?;
                WorldChainSpec::from_genesis(genesis)
            }
            (Some(_), Some(_)) => bail!("set only one of `chain.spec` or `chain.genesis`"),
            (None, None) => bail!("set one of `chain.spec` or `chain.genesis`"),
        };

        if let Some(chain_id) = self.chain.chain_id {
            spec.inner.chain = Chain::from_id(chain_id);
            spec.inner.genesis.config.chain_id = chain_id;
        }

        Ok(spec)
    }

    /// The target commitments exposed to acceptance tests.
    pub fn commitment(&self) -> ManifestCommitment {
        ManifestCommitment::new(self.hardfork, self.features.iter().copied())
    }

    /// Resolve a (possibly relative) path against the manifest directory.
    fn resolve(&self, path: &Path) -> PathBuf {
        match (&self.base_dir, path.is_relative()) {
            (Some(dir), true) => dir.join(path),
            _ => path.to_path_buf(),
        }
    }

    /// Validate static schema invariants.
    pub fn validate(&self) -> Result<()> {
        match (&self.chain.spec, &self.chain.genesis) {
            (Some(_), Some(_)) => bail!("set only one of `chain.spec` or `chain.genesis`"),
            (None, None) => bail!("set one of `chain.spec` or `chain.genesis`"),
            _ => {}
        }

        let mut seen = BTreeSet::new();
        for feature in &self.features {
            if !seen.insert(*feature) {
                bail!("duplicate feature `{feature}`");
            }
        }

        Ok(())
    }
}

/// Serde glue for `hardfork`: serialize and deserialize by stable lowercase key.
mod hardfork_serde {
    use std::str::FromStr;

    use crate::WorldChainHardfork;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    use super::hardfork_key;

    pub fn serialize<S: Serializer>(
        fork: &WorldChainHardfork,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(hardfork_key(*fork))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<WorldChainHardfork, D::Error> {
        let name = String::deserialize(deserializer)?;
        WorldChainHardfork::from_str(&name)
            .map_err(|_| D::Error::custom(format!("unknown World Chain hardfork `{name}`")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
        name = "alphanet"
        hardfork = "jovian"
        features = ["flashblocks", "block_access_list", "pbh"]

        [chain]
        spec = "dev"
        chain_id = 2151908
    "#;

    #[test]
    fn parses_precise_schema() {
        let manifest = NetworkManifest::from_toml_str(SAMPLE).unwrap();
        assert_eq!(manifest.name, "alphanet");
        assert_eq!(manifest.hardfork, WorldChainHardfork::Jovian);
        assert_eq!(
            manifest.features,
            vec![Feature::Flashblocks, Feature::BlockAccessList, Feature::Pbh]
        );
    }

    #[test]
    fn commitment_indexes_features_and_cumulative_hardforks() {
        let manifest = NetworkManifest::from_toml_str(SAMPLE).unwrap();
        let commitment = manifest.commitment();

        assert!(commitment.includes_hardfork(WorldChainHardfork::Jovian));
        assert!(commitment.includes_hardfork(WorldChainHardfork::Bedrock));
        assert!(!commitment.includes_hardfork(WorldChainHardfork::Tropo));

        assert!(commitment.includes_feature(Feature::Flashblocks));
        assert!(commitment.includes_feature(Feature::BlockAccessList));
        assert_eq!(
            commitment.committed_keys(),
            vec![
                "hardfork:jovian".to_string(),
                "feature:flashblocks".to_string(),
                "feature:block_access_list".to_string(),
                "feature:pbh".to_string(),
            ]
        );
    }

    #[test]
    fn chain_id_override_applies() {
        let manifest = NetworkManifest::from_toml_str(SAMPLE).unwrap();
        assert_eq!(
            manifest.into_chain_spec().unwrap().inner.chain.id(),
            2151908
        );
    }

    #[test]
    fn rejects_unknown_feature() {
        let toml = r#"
            name = "broken"
            hardfork = "jovian"
            features = ["teleport"]
            [chain]
            spec = "dev"
        "#;
        assert!(NetworkManifest::from_toml_str(toml).is_err());
    }

    #[test]
    fn rejects_unknown_hardfork() {
        let toml = r#"
            name = "broken"
            hardfork = "karst"
            [chain]
            spec = "dev"
        "#;
        assert!(NetworkManifest::from_toml_str(toml).is_err());
    }

    #[test]
    fn rejects_duplicate_features() {
        let toml = r#"
            name = "broken"
            hardfork = "jovian"
            features = ["flashblocks", "flashblocks"]
            [chain]
            spec = "dev"
        "#;
        assert!(NetworkManifest::from_toml_str(toml).is_err());
    }
}
