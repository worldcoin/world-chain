//! Network manifest: the committed configuration that is the single source of
//! truth for both a World Chain deployment and the acceptance criteria it is
//! tested against.
//!
//! A manifest is a small TOML document that commits to a canonical chain spec
//! (a named OP/World spec or a canonical [`Genesis`] file) plus the World Chain
//! features layered on top. The node consumes it (via `--chain <manifest>.toml`)
//! to derive its chain spec, and the acceptance harness consumes the same file
//! to decide which checks to run and to assert the live network delivers
//! everything the manifest commits to.
//!
//! Hardforks are *not* modelled with a bespoke schedule: they are read back from
//! the derived chain spec through reth's canonical [`Hardforks`] /
//! [`EthereumHardforks`](reth_chainspec::EthereumHardforks) traits, so the
//! manifest commits to canonical chain configuration rather than reinventing it.
//!
//! ```toml
//! name = "alphanet"
//!
//! [chain]
//! spec = "dev"          # a named OP/World spec, or `genesis = "path.json"`
//! chain_id = 2151908    # optional override
//!
//! [features.flashblocks]
//! enabled = true
//! access_list = true
//!
//! [features.pbh]
//! enabled = true
//! verified_blockspace_capacity = 70
//! ```

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use alloy_chains::Chain;
use alloy_genesis::Genesis;
use eyre::eyre::{Result, WrapErr, bail, eyre};
use reth_chainspec::{ForkCondition, Hardforks};
use serde::{Deserialize, Serialize};
use world_chain_chainspec::WorldChainSpec;

mod requirement;

pub use requirement::{FEATURE_KEYS, RequirementKey, feature_keys};

/// A committed network manifest.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkManifest {
    /// Human-readable network name (e.g. `alphanet`).
    pub name: String,
    /// Canonical chain configuration this network commits to.
    #[serde(default)]
    pub chain: ChainConfig,
    /// Committed World Chain features.
    #[serde(default)]
    pub features: Features,
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

/// Committed World Chain features.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Features {
    /// Flashblocks configuration.
    #[serde(default)]
    pub flashblocks: Flashblocks,
    /// Priority Blockspace for Humans configuration.
    #[serde(default)]
    pub pbh: Pbh,
}

/// Flashblocks feature commitment.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flashblocks {
    /// Whether flashblocks are served.
    #[serde(default)]
    pub enabled: bool,
    /// Whether block access lists (BAL) are produced.
    #[serde(default)]
    pub access_list: bool,
}

/// PBH feature commitment.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pbh {
    /// Whether PBH is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Percentage of block gas reserved for verified transactions.
    #[serde(default)]
    pub verified_blockspace_capacity: Option<u8>,
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
        // Eagerly resolve the chain spec so a broken chain source fails at load.
        manifest.into_chain_spec().wrap_err_with(|| {
            format!(
                "manifest {} does not resolve to a chain spec",
                path.display()
            )
        })?;
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

    /// The requirement keys this manifest commits to: every hardfork the derived
    /// chain spec schedules, plus every enabled feature.
    pub fn committed_requirements(&self) -> Result<BTreeSet<String>> {
        let spec = self.into_chain_spec()?;
        let mut keys = scheduled_hardfork_keys(&spec);
        keys.extend(self.committed_feature_keys());
        Ok(keys)
    }

    /// Every recognised requirement key for this manifest: every hardfork the
    /// chain spec knows about (whether scheduled or not), plus every feature
    /// key. Used to reject typos in `requires(...)` declarations.
    pub fn known_requirement_keys(&self) -> Result<BTreeSet<String>> {
        let spec = self.into_chain_spec()?;
        let mut keys: BTreeSet<String> = spec
            .forks_iter()
            .map(|(fork, _)| fork.name().to_ascii_lowercase())
            .collect();
        for feature in FEATURE_KEYS {
            keys.insert(feature.as_str().to_string());
        }
        Ok(keys)
    }

    /// Whether the manifest commits to a given requirement key.
    pub fn commits_to(&self, key: &str) -> Result<bool> {
        Ok(self.committed_requirements()?.contains(key))
    }

    /// Feature requirement keys this manifest enables.
    fn committed_feature_keys(&self) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        if self.features.flashblocks.enabled {
            keys.insert(RequirementKey::Flashblocks.as_str().to_string());
            if self.features.flashblocks.access_list {
                keys.insert(RequirementKey::FlashblocksAccessList.as_str().to_string());
            }
        }
        if self.features.pbh.enabled {
            keys.insert(RequirementKey::Pbh.as_str().to_string());
        }
        keys
    }

    /// Resolve a (possibly relative) path against the manifest directory.
    fn resolve(&self, path: &Path) -> PathBuf {
        match (&self.base_dir, path.is_relative()) {
            (Some(dir), true) => dir.join(path),
            _ => path.to_path_buf(),
        }
    }

    /// Validate internal consistency: a resolvable chain source and coherent
    /// feature parameters.
    pub fn validate(&self) -> Result<()> {
        match (&self.chain.spec, &self.chain.genesis) {
            (Some(_), Some(_)) => bail!("set only one of `chain.spec` or `chain.genesis`"),
            (None, None) => bail!("set one of `chain.spec` or `chain.genesis`"),
            _ => {}
        }

        if self.features.flashblocks.access_list && !self.features.flashblocks.enabled {
            bail!("features.flashblocks.access_list requires features.flashblocks.enabled");
        }
        if let Some(capacity) = self.features.pbh.verified_blockspace_capacity
            && capacity > 100
        {
            bail!(
                "features.pbh.verified_blockspace_capacity must be between 0 and 100, got {capacity}"
            );
        }
        if self.features.pbh.verified_blockspace_capacity.is_some() && !self.features.pbh.enabled {
            bail!(
                "features.pbh.verified_blockspace_capacity set while features.pbh.enabled is false"
            );
        }

        Ok(())
    }
}

/// Lowercase names of every hardfork the spec schedules (condition != `Never`).
fn scheduled_hardfork_keys(spec: &WorldChainSpec) -> BTreeSet<String> {
    spec.forks_iter()
        .filter(|(_, condition)| !matches!(condition, ForkCondition::Never))
        .map(|(fork, _)| fork.name().to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
        name = "alphanet"

        [chain]
        spec = "dev"
        chain_id = 2151908

        [features.flashblocks]
        enabled = true
        access_list = false

        [features.pbh]
        enabled = true
        verified_blockspace_capacity = 70
    "#;

    #[test]
    fn derives_requirements_from_canonical_spec() {
        let manifest = NetworkManifest::from_toml_str(SAMPLE).unwrap();
        let reqs = manifest.committed_requirements().unwrap();

        // Hardforks come straight from the canonical chain spec.
        assert!(reqs.contains("jovian"));
        assert!(reqs.contains("cancun"));
        // Features are our addition.
        assert!(reqs.contains("flashblocks"));
        assert!(!reqs.contains("flashblocks_access_list"));
        assert!(reqs.contains("pbh"));

        assert!(manifest.commits_to("flashblocks").unwrap());
        assert!(
            manifest
                .known_requirement_keys()
                .unwrap()
                .contains("jovian")
        );
    }

    #[test]
    fn derives_chain_spec_with_chain_id_override() {
        let manifest = NetworkManifest::from_toml_str(SAMPLE).unwrap();
        let spec = manifest.into_chain_spec().unwrap();
        assert_eq!(spec.inner.chain.id(), 2151908);
    }

    #[test]
    fn rejects_missing_chain_source() {
        let toml = r#"name = "broken""#;
        assert!(NetworkManifest::from_toml_str(toml).is_err());
    }

    #[test]
    fn rejects_access_list_without_flashblocks() {
        let toml = r#"
            name = "broken"
            [chain]
            spec = "dev"
            [features.flashblocks]
            enabled = false
            access_list = true
        "#;
        assert!(NetworkManifest::from_toml_str(toml).is_err());
    }
}
