//! `xtask acceptance` — run the manifest-driven acceptance harness against a
//! freshly spawned devnet or a remote alphanet, and emit an acceptance report.
//!
//! The same manifest both configures the target (the spawned devnet's forks and
//! features are derived from it) and is the acceptance criterion, so a network
//! is tested against exactly what it commits to. The optional `--fork-matrix`
//! sweep provisions one devnet per hardfork and aggregates the results.

use std::{future::Future, path::PathBuf, pin::Pin, str::FromStr, sync::Arc, time::Duration};

use clap::{Args as ClapArgs, ValueEnum};
use eyre::eyre::{Result, WrapErr, bail};
use tokio::sync::watch;
use tracing::{info, warn};
use url::Url;
use world_chain_acceptance::{
    AcceptanceTarget, CloudflareAccess, Env, Feature, JwtSecret, MatrixCell, NetworkManifest,
    Provisioned, Remote, RemoteConfig, Report, RunOptions, Teardown, Thresholds,
    WORLD_CHAIN_HARDFORKS, WorldChainHardfork, run_matrix,
};

use world_chain_devnet::{
    HaSequencerConfig, ObservabilityConfig, WorldChainHardforkConfig, WorldDevnetBuilder,
    WorldDevnetPreset, is_docker_unavailable,
};

/// Run the acceptance harness and emit a report.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Path to the committed network manifest (the acceptance criterion).
    #[arg(long)]
    manifest: PathBuf,

    /// What to run against.
    #[arg(long, value_enum, default_value_t = Target::Spawned)]
    target: Target,

    /// L2 RPC URL for `--target alphanet` (falls back to `ACCEPTANCE_RPC_URL`).
    #[arg(long, env = "ACCEPTANCE_RPC_URL")]
    rpc_url: Option<Url>,

    /// Optional L1 RPC URL. Enables the safe-head health check.
    #[arg(long, env = "ACCEPTANCE_L1_RPC_URL")]
    l1_rpc_url: Option<Url>,

    /// Optional Engine API (authrpc) URL. Requires `--engine-jwt`.
    #[arg(long, env = "ACCEPTANCE_ENGINE_RPC_URL")]
    engine_rpc_url: Option<Url>,

    /// Engine API JWT secret as a hex string. Sourced from the environment so
    /// it never lands in shell history; never logged.
    #[arg(long, env = "ACCEPTANCE_ENGINE_JWT", hide_env_values = true)]
    engine_jwt: Option<String>,

    /// Optional flashblocks endpoint.
    #[arg(long, env = "ACCEPTANCE_FLASHBLOCKS_URL")]
    flashblocks_url: Option<Url>,

    /// Optional Prometheus endpoint.
    #[arg(long, env = "ACCEPTANCE_PROMETHEUS_URL")]
    prometheus_url: Option<Url>,

    /// Network label override (defaults to the manifest name).
    #[arg(long)]
    network: Option<String>,

    /// Run only tests whose Rust module/package path contains this substring.
    #[arg(long)]
    package: Option<String>,

    /// Run only tests whose name contains this substring.
    #[arg(long)]
    name: Option<String>,

    /// Maximum number of non-serial tests to run concurrently within a cell.
    #[arg(long)]
    concurrency: Option<usize>,

    /// Sweep the suite across multiple hardforks (spawned target only). Accepts
    /// `all`, `through:<fork>`, or a comma list like `jovian,tropo,strato`. Each
    /// cell provisions a fresh devnet at that fork with the manifest's features.
    #[arg(long)]
    fork_matrix: Option<String>,

    /// Write the JSON report to this path.
    #[arg(long)]
    report: Option<PathBuf>,

    /// Write the Markdown report to this path.
    #[arg(long)]
    markdown: Option<PathBuf>,

    /// Write the JUnit XML report to this path.
    #[arg(long)]
    junit: Option<PathBuf>,

    /// Spawned-devnet topology preset (`--target spawned` only).
    #[arg(long, value_enum, default_value_t = PresetArg::HaSequencer)]
    preset: PresetArg,

    /// Spawned-devnet block production interval in milliseconds.
    #[arg(long, default_value_t = 1000)]
    block_time_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Target {
    /// Spawn an in-process devnet derived from the manifest, run the suite, then
    /// tear it down.
    Spawned,
    /// Connect to a remote alphanet over RPC.
    Alphanet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum PresetArg {
    DirectSequencer,
    Minimal,
    HaSequencer,
}

impl From<PresetArg> for WorldDevnetPreset {
    fn from(value: PresetArg) -> Self {
        match value {
            PresetArg::DirectSequencer => Self::DirectSequencer,
            PresetArg::Minimal => Self::Minimal,
            PresetArg::HaSequencer => Self::HaSequencer,
        }
    }
}

pub async fn run_acceptance(args: Args) -> Result<()> {
    let manifest = Arc::new(
        NetworkManifest::load(&args.manifest)
            .wrap_err_with(|| format!("failed to load manifest {}", args.manifest.display()))?,
    );
    info!(
        manifest = %args.manifest.display(),
        network = %manifest.name,
        "loaded acceptance manifest"
    );

    let options = run_options(&args);
    let cells = matrix_cells(&args, &manifest)?;
    let engine_jwt = parse_engine_jwt(args.engine_jwt.as_deref())?;

    let report = match args.target {
        Target::Spawned => {
            let target = SpawnedDevnet::new(&args, manifest, engine_jwt);
            match run_matrix(&target, &cells, &options).await {
                Ok(report) => Some(report),
                Err(err) if is_docker_unavailable(&err) => {
                    warn!(error = %format!("{err:#}"), "skipping spawned acceptance run: Docker is unavailable");
                    None
                }
                Err(err) => return Err(err),
            }
        }
        Target::Alphanet => {
            let target = remote_target(&args, manifest, engine_jwt)?;
            Some(run_matrix(&target, &cells, &options).await?)
        }
    };

    let Some(report) = report else {
        // Spawned target was skipped because Docker is unavailable.
        return Ok(());
    };

    emit_report(&args, &report)?;

    if report.passed() {
        info!(
            passed = report.totals.passed,
            skipped = report.totals.skipped,
            "acceptance run passed"
        );
        Ok(())
    } else {
        bail!(
            "acceptance run failed: {} of {} tests failed",
            report.totals.failed,
            report.totals.total
        );
    }
}

fn run_options(args: &Args) -> RunOptions {
    RunOptions {
        package_filter: args.package.clone(),
        name_filter: args.name.clone(),
        concurrency: args.concurrency,
    }
}

/// Resolve the matrix cells for this run. A non-matrix run is a single cell
/// taken from the manifest. The fork-matrix sweep is spawned-target only.
fn matrix_cells(args: &Args, manifest: &NetworkManifest) -> Result<Vec<MatrixCell>> {
    let Some(spec) = &args.fork_matrix else {
        return Ok(vec![MatrixCell::from_manifest(manifest)]);
    };

    if args.target != Target::Spawned {
        bail!(
            "--fork-matrix is only valid for `--target spawned`; a remote network is a single deployed fork"
        );
    }

    let features: Vec<Feature> = manifest.features.clone();
    let forks = parse_fork_set(spec)?;
    Ok(forks
        .into_iter()
        .map(|fork| MatrixCell::new(fork, features.iter().copied()))
        .collect())
}

/// Parse a `--fork-matrix` spec into an ordered, deduplicated fork set.
fn parse_fork_set(spec: &str) -> Result<Vec<WorldChainHardfork>> {
    let spec = spec.trim();
    if spec.eq_ignore_ascii_case("all") {
        return Ok(WORLD_CHAIN_HARDFORKS.to_vec());
    }
    if let Some(name) = spec.strip_prefix("through:") {
        let target = WorldChainHardfork::from_str(name.trim())
            .map_err(|_| eyre::eyre::eyre!("unknown hardfork `{name}` in --fork-matrix"))?;
        return Ok(WORLD_CHAIN_HARDFORKS
            .iter()
            .copied()
            .filter(|fork| fork.idx() <= target.idx())
            .collect());
    }
    spec.split(',')
        .map(|name| {
            WorldChainHardfork::from_str(name.trim())
                .map_err(|_| eyre::eyre::eyre!("unknown hardfork `{name}` in --fork-matrix"))
        })
        .collect()
}

/// A spawned-devnet [`AcceptanceTarget`]: each cell builds a fresh
/// `world_chain_devnet` at the cell's hardfork and features.
struct SpawnedDevnet {
    manifest: Arc<NetworkManifest>,
    preset: WorldDevnetPreset,
    block_time: Duration,
    network: Option<String>,
    engine_rpc_url: Option<Url>,
    engine_jwt: Option<JwtSecret>,
}

impl SpawnedDevnet {
    fn new(args: &Args, manifest: Arc<NetworkManifest>, engine_jwt: Option<JwtSecret>) -> Self {
        Self {
            manifest,
            preset: WorldDevnetPreset::from(args.preset),
            block_time: Duration::from_millis(args.block_time_ms),
            network: args.network.clone(),
            engine_rpc_url: args.engine_rpc_url.clone(),
            engine_jwt,
        }
    }

    async fn provision_inner(&self, cell: MatrixCell) -> Result<Provisioned> {
        let observability = if self.preset == WorldDevnetPreset::HaSequencer {
            ObservabilityConfig::enabled()
        } else {
            ObservabilityConfig::default()
        };
        let ha_config = HaSequencerConfig::default().with_observability(observability.clone());

        let hardforks = WorldChainHardforkConfig::through(cell.hardfork);
        hardforks.validate()?;

        let has_feature = |feature: Feature| cell.features.contains(&feature);

        let devnet = WorldDevnetBuilder::new()
            .preset(self.preset)
            .ha_sequencer(ha_config)
            .observability(observability)
            .hardforks(hardforks)
            .flashblocks(has_feature(Feature::Flashblocks))
            .flashblocks_access_list(has_feature(Feature::BlockAccessList))
            .block_time(self.block_time)
            .build()
            .await?;

        // Capture endpoints before the devnet is moved into its run loop.
        let l2_rpc_url: Url = devnet.sequencer_rpc_url().parse()?;
        let l1_rpc_url = parse_opt_url(devnet.l1_rpc_url())?;
        let flashblocks_url = parse_opt_url(devnet.flashblocks_url().as_deref())?;
        let prometheus_url = parse_opt_url(devnet.prometheus_url())?;

        // Reflect the cell's fork/features in the manifest the report displays.
        let mut cell_manifest = (*self.manifest).clone();
        cell_manifest.hardfork = cell.hardfork;
        cell_manifest.features = cell.features.clone();

        let mut builder = Env::builder(Arc::new(cell_manifest))
            .l2_rpc_url(l2_rpc_url)
            .l1_rpc_url(l1_rpc_url)
            .engine(self.engine_rpc_url.clone(), self.engine_jwt)
            .flashblocks_url(flashblocks_url)
            .prometheus_url(prometheus_url)
            .thresholds(Thresholds {
                block_advance_timeout: Duration::from_secs(120),
                ..Thresholds::default()
            });
        if let Some(network) = &self.network {
            builder = builder.network(network.clone());
        }
        let env = builder.build()?;

        // Drive continuous block production in the background while tests run.
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let devnet_handle =
            tokio::spawn(async move { devnet.run_until_shutdown(shutdown_rx).await });

        let teardown: Teardown = Box::new(move || {
            Box::pin(async move {
                let _ = shutdown_tx.send(true);
                devnet_handle
                    .await
                    .wrap_err("devnet task panicked")?
                    .wrap_err("devnet run loop returned an error")?;
                Ok(())
            })
        });

        Ok(Provisioned::with_teardown(env, teardown))
    }
}

impl AcceptanceTarget for SpawnedDevnet {
    fn manifest(&self) -> Arc<NetworkManifest> {
        self.manifest.clone()
    }

    fn provision(
        &self,
        cell: MatrixCell,
    ) -> Pin<Box<dyn Future<Output = Result<Provisioned>> + Send + '_>> {
        Box::pin(async move { self.provision_inner(cell).await })
    }
}

/// Build the in-crate [`Remote`] target from CLI args and the environment.
fn remote_target(
    args: &Args,
    manifest: Arc<NetworkManifest>,
    engine_jwt: Option<JwtSecret>,
) -> Result<Remote> {
    let Some(rpc_url) = args.rpc_url.clone() else {
        bail!("`--target alphanet` requires `--rpc-url` (or ACCEPTANCE_RPC_URL)");
    };

    let config = RemoteConfig {
        network: args.network.clone(),
        l2_rpc_url: Some(rpc_url),
        l1_rpc_url: args.l1_rpc_url.clone(),
        engine_url: args.engine_rpc_url.clone(),
        engine_jwt,
        flashblocks_url: args.flashblocks_url.clone(),
        prometheus_url: args.prometheus_url.clone(),
        cloudflare_access: cloudflare_access_from_env()?,
        thresholds: None,
    };

    Remote::new(manifest, config)
}

fn emit_report(args: &Args, report: &Report) -> Result<()> {
    print!("{}", report.to_summary());

    if let Some(path) = &args.report {
        std::fs::write(path, report.to_json()?)
            .wrap_err_with(|| format!("failed to write JSON report to {}", path.display()))?;
        info!(path = %path.display(), "wrote JSON acceptance report");
    }
    if let Some(path) = &args.markdown {
        std::fs::write(path, report.to_markdown())
            .wrap_err_with(|| format!("failed to write Markdown report to {}", path.display()))?;
        info!(path = %path.display(), "wrote Markdown acceptance report");
    }
    if let Some(path) = &args.junit {
        std::fs::write(path, report.to_junit())
            .wrap_err_with(|| format!("failed to write JUnit report to {}", path.display()))?;
        info!(path = %path.display(), "wrote JUnit acceptance report");
    }
    Ok(())
}

/// Parse the Engine API JWT secret from a hex string, if provided.
fn parse_engine_jwt(hex: Option<&str>) -> Result<Option<JwtSecret>> {
    hex.map(|hex| JwtSecret::from_hex(hex).wrap_err("failed to parse --engine-jwt as a hex secret"))
        .transpose()
}

fn parse_opt_url(value: Option<&str>) -> Result<Option<Url>> {
    value
        .map(|raw| Url::parse(raw).wrap_err_with(|| format!("failed to parse URL `{raw}`")))
        .transpose()
}

fn cloudflare_access_from_env() -> Result<Option<CloudflareAccess>> {
    let client_id = std::env::var("CF_ACCESS_CLIENT_ID")
        .ok()
        .filter(|v| !v.is_empty());
    let client_secret = std::env::var("CF_ACCESS_CLIENT_SECRET")
        .ok()
        .filter(|v| !v.is_empty());

    match (client_id, client_secret) {
        (Some(client_id), Some(client_secret)) => Ok(Some(CloudflareAccess {
            client_id,
            client_secret,
        })),
        (None, None) => Ok(None),
        _ => bail!("CF_ACCESS_CLIENT_ID and CF_ACCESS_CLIENT_SECRET must be set together"),
    }
}
