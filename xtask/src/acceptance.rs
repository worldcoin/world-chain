//! `xtask acceptance` — run the manifest-driven acceptance harness against a
//! freshly spawned devnet or a remote alphanet, and emit an acceptance report.
//!
//! The same manifest both configures the target (the spawned devnet's forks and
//! features are derived from it) and is the acceptance criterion, so a network
//! is tested against exactly what it commits to.

use std::{collections::BTreeSet, path::PathBuf, sync::Arc, time::Duration};

use clap::{Args as ClapArgs, ValueEnum};
use eyre::eyre::{Result, WrapErr, bail, eyre};
use tokio::sync::watch;
use tracing::{info, warn};
use url::Url;
use world_chain_acceptance::{
    Category, CloudflareAccess, Env, NetworkManifest, Report, RunOptions, Thresholds, run,
};
use world_chain_devnet::{
    HaSequencerConfig, ObservabilityConfig, WORLD_CHAIN_DEVNET_HARDFORK_ORDER,
    WorldChainHardforkConfig, WorldDevnetBuilder, WorldDevnetPreset, is_docker_unavailable,
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

    /// Optional flashblocks endpoint.
    #[arg(long, env = "ACCEPTANCE_FLASHBLOCKS_URL")]
    flashblocks_url: Option<Url>,

    /// Optional Prometheus endpoint.
    #[arg(long, env = "ACCEPTANCE_PROMETHEUS_URL")]
    prometheus_url: Option<Url>,

    /// Network label override (defaults to the manifest name).
    #[arg(long)]
    network: Option<String>,

    /// Restrict to these categories (`health`, `spec`, `performance`).
    #[arg(long, value_delimiter = ',')]
    category: Vec<String>,

    /// Run only checks whose name contains this substring.
    #[arg(long)]
    name: Option<String>,

    /// Write the JSON report to this path.
    #[arg(long)]
    report: Option<PathBuf>,

    /// Write the Markdown report to this path.
    #[arg(long)]
    markdown: Option<PathBuf>,

    /// Fail the run if any committed requirement has no backing check.
    #[arg(long)]
    strict: bool,

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

    let options = run_options(&args)?;

    let report = match args.target {
        Target::Spawned => run_spawned(&args, manifest, &options).await?,
        Target::Alphanet => run_remote(&args, manifest, &options).await?,
    };

    let Some(report) = report else {
        // Spawned target was skipped because Docker is unavailable.
        return Ok(());
    };

    emit_report(&args, &report)?;

    let passed = if args.strict {
        report.passed_strict()
    } else {
        report.passed()
    };

    if passed {
        info!(
            passed = report.totals.passed,
            skipped = report.totals.skipped,
            "acceptance run passed"
        );
        Ok(())
    } else if args.strict && !report.uncovered_commitments.is_empty() && report.passed() {
        bail!(
            "acceptance run failed under --strict: uncovered commitments: {}",
            report.uncovered_commitments.join(", ")
        );
    } else {
        bail!(
            "acceptance run failed: {} of {} checks failed",
            report.totals.failed,
            report.totals.total
        );
    }
}

fn run_options(args: &Args) -> Result<RunOptions> {
    let categories = if args.category.is_empty() {
        None
    } else {
        let mut set = BTreeSet::new();
        for raw in &args.category {
            set.insert(Category::parse(raw).ok_or_else(|| eyre!("unknown category `{raw}`"))?);
        }
        Some(set)
    };

    Ok(RunOptions {
        categories,
        name_filter: args.name.clone(),
    })
}

/// Build a remote environment and run the suite against it.
async fn run_remote(
    args: &Args,
    manifest: Arc<NetworkManifest>,
    options: &RunOptions,
) -> Result<Option<Report>> {
    let Some(rpc_url) = args.rpc_url.clone() else {
        bail!("`--target alphanet` requires `--rpc-url` (or ACCEPTANCE_RPC_URL)");
    };

    let mut builder = Env::builder(manifest)
        .l2_rpc_url(rpc_url)
        .l1_rpc_url(args.l1_rpc_url.clone())
        .flashblocks_url(args.flashblocks_url.clone())
        .prometheus_url(args.prometheus_url.clone())
        .cloudflare_access(cloudflare_access_from_env()?);
    if let Some(network) = &args.network {
        builder = builder.network(network.clone());
    }

    Ok(Some(run(builder.build()?, options).await))
}

/// Spawn an in-process devnet derived from the manifest, run the suite, tear down.
async fn run_spawned(
    args: &Args,
    manifest: Arc<NetworkManifest>,
    options: &RunOptions,
) -> Result<Option<Report>> {
    let preset = WorldDevnetPreset::from(args.preset);
    let observability = if preset == WorldDevnetPreset::HaSequencer {
        ObservabilityConfig::enabled()
    } else {
        ObservabilityConfig::default()
    };
    let ha_config = HaSequencerConfig::default().with_observability(observability.clone());

    let build = WorldDevnetBuilder::new()
        .preset(preset)
        .ha_sequencer(ha_config)
        .observability(observability)
        .hardforks(hardfork_config(&manifest)?)
        .flashblocks(manifest.features.flashblocks.enabled)
        .flashblocks_access_list(manifest.features.flashblocks.access_list)
        .block_time(Duration::from_millis(args.block_time_ms))
        .build()
        .await;

    let devnet = match build {
        Ok(devnet) => devnet,
        Err(err) if is_docker_unavailable(&err) => {
            warn!(error = %format!("{err:#}"), "skipping spawned acceptance run: Docker is unavailable");
            return Ok(None);
        }
        Err(err) => return Err(err),
    };

    // Capture endpoints before the devnet is moved into its run loop.
    let l2_rpc_url: Url = devnet.sequencer_rpc_url().parse()?;
    let l1_rpc_url = parse_opt_url(devnet.l1_rpc_url())?;
    let flashblocks_url = parse_opt_url(devnet.flashblocks_url().as_deref())?;
    let prometheus_url = parse_opt_url(devnet.prometheus_url())?;

    let mut builder = Env::builder(manifest)
        .l2_rpc_url(l2_rpc_url)
        .l1_rpc_url(l1_rpc_url)
        .flashblocks_url(flashblocks_url)
        .prometheus_url(prometheus_url)
        .thresholds(Thresholds {
            block_advance_timeout: Duration::from_secs(120),
            ..Thresholds::default()
        });
    if let Some(network) = &args.network {
        builder = builder.network(network.clone());
    }
    let env = builder.build()?;

    // Drive continuous block production in the background while checks run.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let devnet_handle = tokio::spawn(async move { devnet.run_until_shutdown(shutdown_rx).await });

    let report = run(env, options).await;

    let _ = shutdown_tx.send(true);
    devnet_handle
        .await
        .wrap_err("devnet task panicked")?
        .wrap_err("devnet run loop returned an error")?;

    Ok(Some(report))
}

/// Map the manifest's committed hardforks onto the devnet hardfork config.
///
/// The manifest commits to a canonical chain spec; we read which World Chain
/// forks it schedules (by requirement key) and reflect them in the devnet's
/// fork selection so the spawned devnet runs exactly what the manifest commits.
fn hardfork_config(manifest: &NetworkManifest) -> Result<WorldChainHardforkConfig> {
    let committed = manifest.committed_requirements()?;
    let mut config = WorldChainHardforkConfig::default();
    for fork in WORLD_CHAIN_DEVNET_HARDFORK_ORDER {
        let key = format!("{fork:?}").to_ascii_lowercase();
        config = if committed.contains(&key) {
            config.enable(fork)
        } else {
            config.disable(fork)
        };
    }
    config.validate()?;
    Ok(config)
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
    Ok(())
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
