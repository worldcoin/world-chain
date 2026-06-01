//! Network manifest: the committed configuration that is the single source of
//! truth for both a World Chain deployment and the acceptance criteria it is
//! tested against.
//!
//! A manifest commits to a canonical chain spec (a named OP/World spec or a
//! canonical [`Genesis`] file), exactly one **hardfork**, and an arbitrary set
//! of **features**. The node consumes it (via `--chain <manifest>.toml`) to
//! derive its chain spec; the acceptance harness consumes the same file to
//! decide which checks run and to assert the live network delivers everything
//! committed.
//!
//! Hardfork ordering is read from the derived chain spec through reth's
//! canonical [`Hardforks`] trait, so a hardfork requirement is satisfied when
//! the committed hardfork is at or after it (cumulative).
//!
//! ```toml
//! name = "alphanet"
//! hardfork = "jovian"                              # exactly one
//! features = ["flashblocks", "block_access_list"]  # arbitrary set
//!
//! [chain]
//! spec = "dev"          # a named OP/World spec, or `genesis = "path.json"`
//! chain_id = 2151908    # optional override
//! ```

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use alloy_chains::Chain;
use alloy_genesis::Genesis;
use eyre::eyre::{Result, WrapErr, bail, eyre};
use reth_chainspec::{ForkCondition, Hardforks};
use serde::{Deserialize, Serialize};
use world_chain_chainspec::WorldChainSpec;

mod requirement;

pub use requirement::{Feature, Hardfork, KNOWN_FEATURE_KEYS, Requirement, feature_is_known};

/// A committed network manifest.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkManifest {
    /// Human-readable network name (e.g. `alphanet`).
    pub name: String,
    /// The single hardfork this network commits to (canonical fork name).
    pub hardfork: String,
    /// The set of features this network commits to.
    #[serde(default)]
    pub features: Vec<String>,
    /// Canonical chain configuration this network commits to.
    #[serde(default)]
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

/// The resolved commitments of a manifest: the hardfork (with its position in
/// the canonical fork order) and the feature set. Owns requirement gating.
#[derive(Clone, Debug)]
pub struct Commitments {
    hardfork: String,
    /// Canonical fork name -> position among scheduled forks.
    fork_order: BTreeMap<String, usize>,
    features: BTreeSet<String>,
}

impl Commitments {
    /// Whether the committed configuration satisfies `requirement`.
    ///
    /// Features must be in the committed set; a hardfork is satisfied when the
    /// committed hardfork is at or after it in the canonical order.
    pub fn satisfies(&self, requirement: &Requirement) -> bool {
        match requirement {
            Requirement::Feature(Feature(name)) => self.features.contains(*name),
            Requirement::Hardfork(Hardfork(name)) => {
                match (self.fork_order.get(*name), self.committed_index()) {
                    (Some(required), Some(committed)) => *required <= committed,
                    _ => false,
                }
            }
        }
    }

    /// The committed hardfork name.
    pub fn hardfork(&self) -> &str {
        &self.hardfork
    }

    /// The committed feature keys.
    pub fn features(&self) -> &BTreeSet<String> {
        &self.features
    }

    /// The requirement keys this manifest commits to: the hardfork plus every
    /// feature. Used to flag commitments that no check exercises.
    pub fn committed_keys(&self) -> BTreeSet<String> {
        let mut keys = self.features.clone();
        keys.insert(self.hardfork.clone());
        keys
    }

    fn committed_index(&self) -> Option<usize> {
        self.fork_order.get(&self.hardfork).copied()
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
        // Resolve commitments so a broken chain source or an unscheduled
        // hardfork fails at load.
        manifest.commitments().wrap_err_with(|| {
            format!("manifest {} is not internally consistent", path.display())
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

    /// Resolve the manifest's commitments against its canonical chain spec.
    ///
    /// The committed hardfork must be scheduled by the chain spec; its position
    /// in the canonical fork order drives cumulative hardfork gating.
    pub fn commitments(&self) -> Result<Commitments> {
        let spec = self.into_chain_spec()?;

        let mut fork_order = BTreeMap::new();
        for (index, (fork, _)) in spec
            .forks_iter()
            .filter(|(_, condition)| !matches!(condition, ForkCondition::Never))
            .enumerate()
        {
            fork_order.insert(fork.name().to_ascii_lowercase(), index);
        }

        let hardfork = self.hardfork.to_ascii_lowercase();
        if !fork_order.contains_key(&hardfork) {
            bail!(
                "committed hardfork `{}` is not scheduled by the chain spec",
                self.hardfork
            );
        }

        let features = self
            .features
            .iter()
            .map(|f| f.to_ascii_lowercase())
            .collect();

        Ok(Commitments {
            hardfork,
            fork_order,
            features,
        })
    }

    /// Resolve a (possibly relative) path against the manifest directory.
    fn resolve(&self, path: &Path) -> PathBuf {
        match (&self.base_dir, path.is_relative()) {
            (Some(dir), true) => dir.join(path),
            _ => path.to_path_buf(),
        }
    }

    /// Validate static internal consistency (cheap checks that do not need the
    /// chain spec). Chain-spec consistency is enforced by [`Self::commitments`].
    pub fn validate(&self) -> Result<()> {
        match (&self.chain.spec, &self.chain.genesis) {
            (Some(_), Some(_)) => bail!("set only one of `chain.spec` or `chain.genesis`"),
            (None, None) => bail!("set one of `chain.spec` or `chain.genesis`"),
            _ => {}
        }

        if self.hardfork.trim().is_empty() {
            bail!("`hardfork` must name the single committed hardfork");
        }

        let mut seen = BTreeSet::new();
        for feature in &self.features {
            let key = feature.to_ascii_lowercase();
            if !feature_is_known(&key) {
                bail!(
                    "unknown feature `{feature}`; expected one of {}",
                    KNOWN_FEATURE_KEYS.join(", ")
                );
            }
            if !seen.insert(key) {
                bail!("duplicate feature `{feature}`");
            }
        }

        let has = |key: &str| seen.contains(key);
        if has("block_access_list") && !has("flashblocks") {
            bail!("feature `block_access_list` requires feature `flashblocks`");
        }

        Ok(())
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
    fn cumulative_hardfork_and_feature_gating() {
        let manifest = NetworkManifest::from_toml_str(SAMPLE).unwrap();
        let commitments = manifest.commitments().unwrap();

        // Cumulative: forks at or before the committed hardfork are satisfied.
        assert!(commitments.satisfies(&Requirement::hardfork("jovian")));
        assert!(commitments.satisfies(&Requirement::hardfork("cancun")));
        // A later / unscheduled fork is not.
        assert!(!commitments.satisfies(&Requirement::hardfork("karst")));
        assert!(!commitments.satisfies(&Requirement::hardfork("tropo")));

        // Features must be in the committed set.
        assert!(commitments.satisfies(&Requirement::feature("flashblocks")));
        assert!(commitments.satisfies(&Requirement::feature("block_access_list")));
    }

    #[test]
    fn pbh_feature_is_committed() {
        let manifest = NetworkManifest::from_toml_str(SAMPLE).unwrap();
        let commitments = manifest.commitments().unwrap();
        assert!(commitments.satisfies(&Requirement::feature("pbh")));
        assert!(commitments.committed_keys().contains("jovian"));
        assert!(commitments.committed_keys().contains("flashblocks"));
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
    fn rejects_unscheduled_hardfork() {
        let toml = r#"
            name = "broken"
            hardfork = "tropo"
            [chain]
            spec = "dev"
        "#;
        // Parses, but commitments fail because `dev` does not schedule tropo.
        let manifest = NetworkManifest::from_toml_str(toml).unwrap();
        assert!(manifest.commitments().is_err());
    }

    #[test]
    fn rejects_bal_without_flashblocks() {
        let toml = r#"
            name = "broken"
            hardfork = "jovian"
            features = ["block_access_list"]
            [chain]
            spec = "dev"
        "#;
        assert!(NetworkManifest::from_toml_str(toml).is_err());
    }
}
