//! The composer seam: how an [`Env`] is provisioned and torn down.
//!
//! This is the layer that lets one acceptance suite run against very different
//! backends without the runner caring which. It mirrors Optimism devstack's
//! split between the typed per-test frontend ([`Env`]) and the runtime composer
//! that owns ports, dialing, and lifecycle: a [`AcceptanceTarget`] hands the
//! runner a provisioned [`Env`] plus an async teardown, and the runner stays
//! oblivious to whether that env is a spawned Docker devnet, a remote alphanet,
//! or (in future) a fast in-process node.
//!
//! The heavy spawned-devnet target lives in `xtask` (it depends on
//! `world_chain_devnet`); keeping the trait and the lightweight [`Remote`]
//! target here keeps this crate free of the Docker/devnet dependency.

use std::{future::Future, pin::Pin, sync::Arc};

use reth_rpc_layer::JwtSecret;
use url::Url;
use world_chain_chainspec::{Feature, ManifestCommitment, NetworkManifest, WorldChainHardfork};

use crate::env::{CloudflareAccess, Env, Thresholds};

/// One point in the fork × feature space the suite is run against.
///
/// A single acceptance run executes one cell; the fork-matrix sweep executes a
/// sequence of cells, provisioning and tearing down a target for each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatrixCell {
    /// The hardfork the network under test commits to for this cell.
    pub hardfork: WorldChainHardfork,
    /// The features the network under test commits to for this cell.
    pub features: Vec<Feature>,
}

impl MatrixCell {
    /// Construct a cell from a hardfork and feature set.
    pub fn new(hardfork: WorldChainHardfork, features: impl IntoIterator<Item = Feature>) -> Self {
        Self {
            hardfork,
            features: features.into_iter().collect(),
        }
    }

    /// The cell described by a committed manifest.
    pub fn from_manifest(manifest: &NetworkManifest) -> Self {
        Self::new(manifest.hardfork, manifest.features.iter().copied())
    }

    /// The manifest commitment this cell represents, used to gate tests.
    pub fn commitment(&self) -> ManifestCommitment {
        ManifestCommitment::new(self.hardfork, self.features.iter().copied())
    }

    /// A stable label for reports, e.g. `jovian` or `jovian+flashblocks,pbh`.
    pub fn label(&self) -> String {
        let fork = world_chain_chainspec::hardfork_key(self.hardfork);
        if self.features.is_empty() {
            fork.to_string()
        } else {
            let features = self
                .features
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            format!("{fork}+{features}")
        }
    }
}

/// Future returned by an async teardown.
pub type TeardownFuture = Pin<Box<dyn Future<Output = eyre::Result<()>> + Send>>;

/// An owned, run-once teardown for a provisioned environment.
pub type Teardown = Box<dyn FnOnce() -> TeardownFuture + Send>;

/// A live [`Env`] paired with its teardown.
///
/// Teardown is explicit and async (a spawned devnet must signal shutdown and
/// await its run loop), so it is run by the runner after a cell completes rather
/// than relying on `Drop`.
pub struct Provisioned {
    /// The environment the suite runs against.
    pub env: Env,
    teardown: Option<Teardown>,
}

impl Provisioned {
    /// A provisioned env with no teardown (e.g. a remote endpoint we do not own).
    pub fn new(env: Env) -> Self {
        Self {
            env,
            teardown: None,
        }
    }

    /// A provisioned env with an async teardown to run when the cell completes.
    pub fn with_teardown(env: Env, teardown: Teardown) -> Self {
        Self {
            env,
            teardown: Some(teardown),
        }
    }

    /// Run the teardown, if any.
    pub async fn teardown(self) -> eyre::Result<()> {
        match self.teardown {
            Some(teardown) => teardown().await,
            None => Ok(()),
        }
    }

    /// Split into the environment and its (optional) teardown, so the caller can
    /// use the env and run teardown separately.
    pub fn into_parts(self) -> (Env, Option<Teardown>) {
        (self.env, self.teardown)
    }
}

/// A backend that can provision an [`Env`] for a given [`MatrixCell`].
pub trait AcceptanceTarget: Send + Sync {
    /// The base manifest this target was configured from.
    fn manifest(&self) -> Arc<NetworkManifest>;

    /// Provision an environment for `cell`. Implementations that cannot vary the
    /// fork/feature set at runtime (e.g. a remote deployment) ignore `cell`
    /// beyond using it as the gating commitment.
    fn provision(
        &self,
        cell: MatrixCell,
    ) -> Pin<Box<dyn Future<Output = eyre::Result<Provisioned>> + Send + '_>>;
}

/// Connection details for a [`Remote`] target.
///
/// Deliberately not `Debug`: it carries the Engine API JWT and Cloudflare Access
/// secret, which must never be logged.
#[derive(Clone, Default)]
pub struct RemoteConfig {
    /// Network label override (defaults to the manifest name).
    pub network: Option<String>,
    /// Required L2 JSON-RPC endpoint.
    pub l2_rpc_url: Option<Url>,
    /// Optional L1 JSON-RPC endpoint.
    pub l1_rpc_url: Option<Url>,
    /// Optional Engine API endpoint.
    pub engine_url: Option<Url>,
    /// Optional Engine API JWT secret.
    pub engine_jwt: Option<JwtSecret>,
    /// Optional flashblocks endpoint.
    pub flashblocks_url: Option<Url>,
    /// Optional Prometheus endpoint.
    pub prometheus_url: Option<Url>,
    /// Optional Cloudflare Access service-token credentials.
    pub cloudflare_access: Option<CloudflareAccess>,
    /// Optional thresholds override.
    pub thresholds: Option<Thresholds>,
}

/// A target that attaches to an already-running, remotely deployed network.
///
/// Remote networks are a single deployed fork, so the fork-matrix sweep is not
/// applicable; the runner uses exactly one cell derived from the manifest.
pub struct Remote {
    manifest: Arc<NetworkManifest>,
    config: RemoteConfig,
}

impl Remote {
    /// Build a remote target from a manifest and connection details.
    pub fn new(manifest: Arc<NetworkManifest>, config: RemoteConfig) -> eyre::Result<Self> {
        if config.l2_rpc_url.is_none() {
            eyre::eyre::bail!("a remote target requires an L2 RPC URL");
        }
        Ok(Self { manifest, config })
    }
}

impl AcceptanceTarget for Remote {
    fn manifest(&self) -> Arc<NetworkManifest> {
        self.manifest.clone()
    }

    fn provision(
        &self,
        _cell: MatrixCell,
    ) -> Pin<Box<dyn Future<Output = eyre::Result<Provisioned>> + Send + '_>> {
        // Remote provisioning is synchronous (just dials RPC), but the trait is
        // async so spawned backends can await a devnet build.
        let result = self.build_env();
        Box::pin(async move { result.map(Provisioned::new) })
    }
}

impl Remote {
    fn build_env(&self) -> eyre::Result<Env> {
        let cfg = &self.config;
        let l2_rpc_url = cfg
            .l2_rpc_url
            .clone()
            .ok_or_else(|| eyre::eyre::eyre!("a remote target requires an L2 RPC URL"))?;

        let mut builder = Env::builder(self.manifest.clone())
            .l2_rpc_url(l2_rpc_url)
            .l1_rpc_url(cfg.l1_rpc_url.clone())
            .engine(cfg.engine_url.clone(), cfg.engine_jwt)
            .flashblocks_url(cfg.flashblocks_url.clone())
            .prometheus_url(cfg.prometheus_url.clone())
            .cloudflare_access(cfg.cloudflare_access.clone());
        if let Some(network) = &cfg.network {
            builder = builder.network(network.clone());
        }
        if let Some(thresholds) = cfg.thresholds {
            builder = builder.thresholds(thresholds);
        }
        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_label_includes_features() {
        let cell = MatrixCell::new(
            WorldChainHardfork::Jovian,
            [Feature::Flashblocks, Feature::Pbh],
        );
        assert_eq!(cell.label(), "jovian+flashblocks,pbh");
    }

    #[test]
    fn cell_label_omits_empty_features() {
        let cell = MatrixCell::new(WorldChainHardfork::Tropo, []);
        assert_eq!(cell.label(), "tropo");
    }

    #[test]
    fn cell_commitment_matches() {
        let cell = MatrixCell::new(WorldChainHardfork::Strato, [Feature::Flashblocks]);
        let commitment = cell.commitment();
        assert!(commitment.includes_hardfork(WorldChainHardfork::Jovian));
        assert!(commitment.includes_feature(Feature::Flashblocks));
        assert!(!commitment.includes_feature(Feature::Pbh));
    }
}
