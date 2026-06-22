use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    str::FromStr,
    time::{Duration, Instant},
};

use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use eyre::eyre::{Result, bail};
use tokio::sync::watch;
use tracing::{info, warn};
use world_chain_chainspec::WorldChainHardfork;
use world_chain_devnet::{
    DevnetComponent, DevnetPortMode, HaSequencerConfig, ObservabilityConfig,
    WorldChainHardforkConfig, WorldDevnet, WorldDevnetBuilder, WorldDevnetPreset,
};
use world_chain_test_utils::DEV_CHAIN_ID;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

/// Manage the native Rust World Chain devnet.
#[derive(Debug, Parser)]
pub struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start a local devnet and keep it running until Ctrl-C.
    Up(UpArgs),
    /// Stop a background devnet started with `devnet up -d`.
    Down(DownArgs),
}

#[derive(Debug, Clone, ClapArgs)]
struct UpArgs {
    /// Start the devnet in the background.
    #[arg(short = 'd', long)]
    detach: bool,

    /// Topology preset to start.
    #[arg(long, value_enum, default_value_t = PresetArg::HaSequencer)]
    preset: PresetArg,

    /// Use stable host ports where supported.
    #[arg(long)]
    stable_ports: bool,

    /// Disable flashblocks for this run.
    #[arg(long)]
    no_flashblocks: bool,

    /// Enable flashblocks block access lists (BAL) on the sequencer nodes.
    #[arg(long)]
    bal_enabled: bool,

    /// Automatically run a Contender stress test against the live nodes once
    /// the devnet is ready. The devnet keeps running until Ctrl-C.
    #[arg(long)]
    stress: bool,

    /// Target transactions per second for the automated stress run.
    #[arg(long, default_value_t = 50, requires = "stress")]
    stress_tps: u64,

    /// Duration in seconds to sustain the automated stress run.
    #[arg(long, default_value_t = 60, requires = "stress")]
    stress_duration: u64,

    /// Disable the containerized L1 dependency.
    #[arg(long)]
    no_l1: bool,

    /// Enable Prometheus and Grafana for manual debugging.
    #[arg(long)]
    observability: bool,

    /// Disable Prometheus and Grafana, even for HA presets.
    #[arg(long)]
    no_observability: bool,

    /// Number of sequencer replicas for the HA preset.
    #[arg(long, default_value_t = 3)]
    sequencers: u8,

    /// Enable op-challenger for local dispute-game playing.
    ///
    /// This is disabled by default until the native devnet generates matching
    /// Cannon prestates for local games.
    #[arg(long)]
    op_challenger: bool,

    /// Omit op-challenger from the HA topology.
    ///
    /// Kept for compatibility; op-challenger is already disabled by default.
    #[arg(long)]
    no_op_challenger: bool,

    /// Do not deploy the WIP-1006 proof-system contracts in the HA devnet.
    #[arg(long)]
    no_proof_system: bool,

    /// Print the selected topology manifest and exit.
    #[arg(long)]
    print_topology: bool,

    /// Block production interval in milliseconds.
    #[arg(long, default_value_t = 2000)]
    block_time_ms: u64,

    /// Activate all World Chain hardforks through this fork.
    #[arg(long, value_parser = parse_hardfork)]
    latest_hardfork: Option<WorldChainHardfork>,

    /// Enable an individual World Chain hardfork.
    #[arg(long = "enable-hardfork", value_parser = parse_hardfork)]
    enable_hardforks: Vec<WorldChainHardfork>,

    /// Disable an individual World Chain hardfork.
    #[arg(long = "disable-hardfork", value_parser = parse_hardfork)]
    disable_hardforks: Vec<WorldChainHardfork>,
}

#[derive(Debug, Clone, ClapArgs)]
struct DownArgs {
    /// Seconds to wait for graceful shutdown before sending SIGTERM.
    #[arg(long, default_value_t = 30)]
    timeout_secs: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum PresetArg {
    /// One L1 dev chain and one direct-sequencing World Chain node.
    DirectSequencer,
    /// Minimal direct-sequencing single-node setup.
    Minimal,
    /// HA sequencing target topology with op-conductor and observability.
    #[default]
    HaSequencer,
}

impl PresetArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::DirectSequencer => "direct-sequencer",
            Self::Minimal => "minimal",
            Self::HaSequencer => "ha-sequencer",
        }
    }
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

pub async fn run(args: Args) -> Result<()> {
    match args.command {
        Command::Up(args) => up(args).await,
        Command::Down(args) => down(args).await,
    }
}

pub fn should_reset_log(args: &Args) -> bool {
    matches!(
        &args.command,
        Command::Up(args)
            if !args.detach || std::env::var_os("WORLD_CHAIN_DEVNET_BACKGROUND").is_some()
    )
}

async fn up(args: UpArgs) -> Result<()> {
    if args.detach && !args.print_topology {
        return spawn_background(args).await;
    }
    let _pid_file_guard = BackgroundPidFileGuard::from_env();
    remove_file_if_exists(&devnet_endpoints_path())?;

    let mut hardforks = args
        .latest_hardfork
        .map(WorldChainHardforkConfig::through)
        .unwrap_or_default();

    for fork in args.enable_hardforks {
        hardforks = hardforks.enable(fork);
    }
    for fork in args.disable_hardforks {
        hardforks = hardforks.disable(fork);
    }
    hardforks.validate()?;

    let preset = WorldDevnetPreset::from(args.preset);
    let observability = if args.no_observability {
        ObservabilityConfig::default()
    } else if args.observability || preset == WorldDevnetPreset::HaSequencer {
        ObservabilityConfig::enabled()
    } else {
        ObservabilityConfig::default()
    };

    let mut ha_config = HaSequencerConfig::default()
        .with_sequencer_count(args.sequencers)
        .with_op_challenger(args.op_challenger && !args.no_op_challenger)
        .with_observability(observability.clone());
    ha_config.world_contracts.proof_system = !args.no_proof_system;

    let mut builder = WorldDevnetBuilder::new()
        .preset(preset)
        .ha_sequencer(ha_config.clone())
        .observability(observability)
        .hardforks(hardforks)
        .flashblocks(!args.no_flashblocks)
        .flashblocks_access_list(args.bal_enabled)
        .block_time(Duration::from_millis(args.block_time_ms))
        .port_mode(if args.stable_ports {
            DevnetPortMode::Stable
        } else {
            DevnetPortMode::Dynamic
        });

    if args.no_l1 {
        builder = builder.without_l1();
    }

    if args.print_topology {
        if let Some(topology) = builder.ha_topology() {
            print_components(&topology.components);
        } else {
            info!("selected preset has no HA topology manifest");
        }
        return Ok(());
    }

    info!(
        preset = ?args.preset,
        block_time_ms = args.block_time_ms,
        flashblocks = !args.no_flashblocks,
        bal_enabled = args.bal_enabled,
        stress = args.stress,
        stable_ports = args.stable_ports,
        l1 = !args.no_l1,
        observability = !args.no_observability && (args.observability || preset == WorldDevnetPreset::HaSequencer),
        sequencers = args.sequencers,
        op_challenger = args.op_challenger && !args.no_op_challenger,
        proof_system = !args.no_proof_system,
        "Starting native World Chain devnet"
    );

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut ctrl_c = Box::pin(tokio::signal::ctrl_c());
    let devnet = tokio::select! {
        result = builder.build() => result?,
        result = &mut ctrl_c => {
            result?;
            info!("World Chain devnet startup interrupted");
            return Ok(());
        }
    };
    write_endpoints_file(&devnet, &devnet_endpoints_path())?;

    let stress_task = args.stress.then(|| {
        spawn_stress_run(
            devnet.sequencer_rpc_url(),
            args.stress_tps,
            args.stress_duration,
        )
    });

    let signal_task = tokio::spawn(async move {
        match ctrl_c.await {
            Ok(()) => {
                let _ = shutdown_tx.send(true);
            }
            Err(err) => warn!(%err, "failed to listen for Ctrl-C"),
        }
    });

    let result = devnet.run_until_shutdown(shutdown_rx).await;
    signal_task.abort();
    if let Some(task) = stress_task {
        // Dropping the join handle's future kills the stress child via
        // `kill_on_drop` if it is still running.
        task.abort();
    }
    result
}

/// Spawn a background task that drives `scripts/stress/stress.sh` against the
/// live devnet. The script is responsible for `contender setup` + `spam`; we
/// just point it at the primary sequencer and pass the rate/duration through.
fn spawn_stress_run(rpc_url: String, tps: u64, duration: u64) -> tokio::task::JoinHandle<()> {
    let script = stress_script_path();
    tokio::spawn(async move {
        info!(
            rpc_url = %rpc_url,
            tps,
            duration_secs = duration,
            script = %script.display(),
            "starting automated stress run against live devnet"
        );

        let status = tokio::process::Command::new("bash")
            .arg(&script)
            .arg("stress")
            .env("RPC_URL", &rpc_url)
            .env("TPS", tps.to_string())
            .env("DURATION", duration.to_string())
            .kill_on_drop(true)
            .status()
            .await;

        match status {
            Ok(status) if status.success() => info!("automated stress run completed"),
            Ok(status) => warn!(
                ?status,
                "automated stress run exited with a non-zero status"
            ),
            Err(err) => warn!(
                %err,
                "failed to launch automated stress run; is `contender` installed and on PATH?"
            ),
        }
    })
}

fn stress_script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/stress/stress.sh")
}

async fn spawn_background(args: UpArgs) -> Result<()> {
    let pid_path = devnet_pid_path();
    if let Some(pid) = read_pid(&pid_path)? {
        if process_is_running(pid) {
            bail!(
                "devnet already appears to be running with pid {pid}; run `just devnet down` first"
            );
        }
        remove_pid_file(&pid_path)?;
    }

    if let Some(parent) = pid_path.parent() {
        fs::create_dir_all(parent)?;
    }

    reset_devnet_start_files()?;

    let exe = std::env::current_exe()?;
    let log_path = devnet_log_path();
    let mut command = StdCommand::new(exe);
    command
        .args(background_args(&args))
        .env("WORLD_CHAIN_DEVNET_BACKGROUND", "1")
        .env("WORLD_CHAIN_DEVNET_PID_FILE", &pid_path)
        .env("WORLD_CHAIN_DEVNET_LOG_FILE", &log_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    command.process_group(0);

    let child = command.spawn()?;

    let pid = child.id();
    fs::write(&pid_path, pid.to_string())?;

    info!(
        pid,
        pid_file = %pid_path.display(),
        log_file = %log_path.display(),
        endpoints_file = %devnet_endpoints_path().display(),
        "started World Chain devnet in the background"
    );
    Ok(())
}

async fn down(args: DownArgs) -> Result<()> {
    let pid_path = devnet_pid_path();
    let Some(pid) = read_pid(&pid_path)? else {
        info!(
            pid_file = %pid_path.display(),
            "no background World Chain devnet pid file found"
        );
        return Ok(());
    };

    if !process_is_running(pid) {
        remove_pid_file(&pid_path)?;
        info!(
            pid,
            "background World Chain devnet process is not running; removed stale pid file"
        );
        return Ok(());
    }

    info!(pid, "stopping background World Chain devnet");
    signal_process(pid, "INT")?;
    let started_at = Instant::now();
    let timeout = Duration::from_secs(args.timeout_secs);
    while started_at.elapsed() < timeout {
        if !process_is_running(pid) {
            remove_pid_file(&pid_path)?;
            info!(pid, "background World Chain devnet stopped");
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    warn!(
        pid,
        timeout_secs = args.timeout_secs,
        "background devnet did not stop after SIGINT; sending SIGTERM"
    );
    signal_process(pid, "TERM")?;
    remove_pid_file(&pid_path)?;
    Ok(())
}

fn background_args(args: &UpArgs) -> Vec<String> {
    let mut argv = vec![
        "devnet".to_string(),
        "up".to_string(),
        "--preset".to_string(),
        args.preset.as_str().to_string(),
    ];

    if args.stable_ports {
        argv.push("--stable-ports".to_string());
    }
    if args.no_flashblocks {
        argv.push("--no-flashblocks".to_string());
    }
    if args.bal_enabled {
        argv.push("--bal-enabled".to_string());
    }
    if args.stress {
        argv.push("--stress".to_string());
        argv.push("--stress-tps".to_string());
        argv.push(args.stress_tps.to_string());
        argv.push("--stress-duration".to_string());
        argv.push(args.stress_duration.to_string());
    }
    if args.no_l1 {
        argv.push("--no-l1".to_string());
    }
    if args.observability {
        argv.push("--observability".to_string());
    }
    if args.no_observability {
        argv.push("--no-observability".to_string());
    }
    if args.no_op_challenger {
        argv.push("--no-op-challenger".to_string());
    }
    if args.op_challenger {
        argv.push("--op-challenger".to_string());
    }
    if args.no_proof_system {
        argv.push("--no-proof-system".to_string());
    }
    argv.push("--sequencers".to_string());
    argv.push(args.sequencers.to_string());
    argv.push("--block-time-ms".to_string());
    argv.push(args.block_time_ms.to_string());
    if let Some(fork) = args.latest_hardfork {
        argv.push("--latest-hardfork".to_string());
        argv.push(fork.to_string());
    }
    for fork in &args.enable_hardforks {
        argv.push("--enable-hardfork".to_string());
        argv.push(fork.to_string());
    }
    for fork in &args.disable_hardforks {
        argv.push("--disable-hardfork".to_string());
        argv.push(fork.to_string());
    }

    argv
}

fn devnet_pid_path() -> PathBuf {
    std::env::var_os("WORLD_CHAIN_DEVNET_PID_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/devnet/devnet.pid"))
}

fn devnet_log_path() -> PathBuf {
    std::env::var_os("WORLD_CHAIN_DEVNET_LOG_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/devnet/logs/devnet.log"))
}

fn devnet_endpoints_path() -> PathBuf {
    std::env::var_os("WORLD_CHAIN_DEVNET_ENDPOINTS_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/devnet/endpoints.json"))
}

fn reset_devnet_start_files() -> Result<()> {
    remove_file_if_exists(&devnet_log_path())?;
    remove_file_if_exists(&devnet_endpoints_path())?;
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn write_endpoints_file(devnet: &WorldDevnet, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let components: Vec<_> = devnet
        .components()
        .iter()
        .map(|component| {
            serde_json::json!({
                "id": &component.id,
                "kind": component.kind.as_str(),
                "status": component.status.as_str(),
                "image": component.image.as_ref().map(|image| image.reference()),
                "endpoints": component.endpoints.iter().map(|endpoint| {
                    serde_json::json!({
                        "name": &endpoint.name,
                        "url": &endpoint.url,
                    })
                }).collect::<Vec<_>>(),
                "notes": &component.notes,
            })
        })
        .collect();

    let endpoints = serde_json::json!({
        "preset": format!("{:?}", devnet.preset()),
        "chain_id": DEV_CHAIN_ID,
        "primary": {
            "l1_rpc_url": devnet.l1_rpc_url(),
            "l2_rpc_url": devnet.l2_rpc_url(),
            "sequencer_rpc_url": devnet.sequencer_rpc_url(),
            "flashblocks_url": devnet.flashblocks_url(),
            "prometheus_url": devnet.prometheus_url(),
            "grafana_url": devnet.grafana_url(),
        },
        "components": components,
    });

    fs::write(path, serde_json::to_vec_pretty(&endpoints)?)?;
    info!(path = %path.display(), "wrote World Chain devnet endpoints file");
    Ok(())
}

fn read_pid(path: &PathBuf) -> Result<Option<u32>> {
    if !path.exists() {
        return Ok(None);
    }
    let pid = fs::read_to_string(path)?.trim().parse()?;
    Ok(Some(pid))
}

fn remove_pid_file(path: &PathBuf) -> Result<()> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn process_is_running(pid: u32) -> bool {
    StdCommand::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn signal_process(pid: u32, signal: &str) -> Result<()> {
    let target = signal_target(pid);
    let status = StdCommand::new("kill")
        .arg(format!("-{signal}"))
        .arg(&target)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        bail!("failed to send SIG{signal} to pid {pid}")
    }
}

fn signal_target(pid: u32) -> String {
    #[cfg(unix)]
    {
        format!("-{pid}")
    }
    #[cfg(not(unix))]
    {
        pid.to_string()
    }
}

struct BackgroundPidFileGuard {
    path: PathBuf,
}

impl BackgroundPidFileGuard {
    fn from_env() -> Option<Self> {
        std::env::var_os("WORLD_CHAIN_DEVNET_BACKGROUND")?;
        let path = devnet_pid_path();
        Some(Self { path })
    }
}

impl Drop for BackgroundPidFileGuard {
    fn drop(&mut self) {
        let Ok(Some(pid)) = read_pid(&self.path) else {
            return;
        };
        if pid == std::process::id() {
            let _ = remove_pid_file(&self.path);
        }
    }
}

fn parse_hardfork(value: &str) -> std::result::Result<WorldChainHardfork, String> {
    WorldChainHardfork::from_str(value).map_err(|err| err.to_string())
}

fn print_components(components: &[DevnetComponent]) {
    for component in components {
        info!(
            id = %component.id,
            kind = component.kind.as_str(),
            status = component.status.as_str(),
            image = component.image.as_ref().map(|image| image.reference()),
            endpoints = ?component.endpoints,
            notes = ?component.notes,
            "devnet topology component"
        );
    }
}
