use std::{ffi::OsString, fmt, marker::PhantomData};

use clap::Parser;
use eyre::eyre::eyre;
use futures_util::Future;
use reth_cli::chainspec::ChainSpecParser;
use reth_cli_commands::{
    common::{CliComponentsBuilder, CliNodeTypes},
    launcher::{FnLauncher, Launcher},
};
use reth_cli_runner::CliRunner;
use reth_db::DatabaseEnv;
use reth_node_builder::{NodeBuilder, WithLaunchContext};
use reth_node_core::{
    args::{LogArgs, OtlpInitStatus, OtlpLogsStatus, TraceArgs},
    version::version_metadata,
};
use reth_node_metrics::recorder::install_prometheus_recorder;
use reth_rpc_server_types::{DefaultRpcModuleValidator, RethRpcModule, RpcModuleValidator};
use reth_tracing::{Layers, TracingGuards};
use tracing::{info, warn};
use world_chain_chainspec::WorldChainSpec;

use crate::{WorldChainArgs, chainspec::WorldChainSpecParser};

/// Optimism CLI commands, parameterized with the World Chain chain spec parser.
pub use reth_optimism_cli::commands::Commands;

/// The main World Chain cli interface.
#[derive(Debug, Parser)]
#[command(author, name = version_metadata().name_client.as_ref(), version = version_metadata().short_version.as_ref(), long_version = version_metadata().long_version.as_ref(), about = "Reth", long_about = None)]
pub struct Cli<
    Spec: ChainSpecParser = WorldChainSpecParser,
    Ext: clap::Args + fmt::Debug = WorldChainArgs,
    Rpc: RpcModuleValidator = DefaultRpcModuleValidator,
> {
    /// The command to run.
    #[command(subcommand)]
    pub command: Commands<Spec, Ext>,

    /// The logging configuration for the CLI.
    #[command(flatten)]
    pub logs: LogArgs,

    /// The metrics configuration for the CLI.
    #[command(flatten)]
    pub traces: TraceArgs,

    /// Type marker for the RPC module validator.
    #[arg(skip)]
    _phantom: PhantomData<Rpc>,
}

impl Cli {
    /// Parses only the default CLI arguments.
    pub fn parse_args() -> Self {
        Self::parse()
    }

    /// Parses only the default CLI arguments from the given iterator.
    pub fn try_parse_args_from<I, T>(itr: I) -> Result<Self, clap::error::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        Self::try_parse_from(itr)
    }
}

impl<C, Ext, Rpc> Cli<C, Ext, Rpc>
where
    C: ChainSpecParser<ChainSpec = WorldChainSpec>,
    Ext: clap::Args + fmt::Debug,
    Rpc: RpcModuleValidator,
{
    /// Configures the CLI and returns a [`CliApp`] instance.
    pub fn configure(self) -> CliApp<C, Ext, Rpc> {
        CliApp::new(self)
    }

    /// Executes the configured cli command.
    pub fn run<N, L, Fut, CompBuilder>(
        self,
        launcher: L,
        components: CompBuilder,
    ) -> eyre::Result<()>
    where
        N: CliNodeTypes<ChainSpec = C::ChainSpec>,
        L: FnOnce(WithLaunchContext<NodeBuilder<DatabaseEnv, C::ChainSpec>>, Ext) -> Fut,
        Fut: Future<Output = eyre::Result<()>>,
        CompBuilder: CliComponentsBuilder<N>,
    {
        self.with_runner::<N, _, _, _>(CliRunner::try_default_runtime()?, launcher, components)
    }

    /// Executes the configured cli command with the provided [`CliRunner`].
    pub fn with_runner<N, L, Fut, CompBuilder>(
        self,
        runner: CliRunner,
        launcher: L,
        components: CompBuilder,
    ) -> eyre::Result<()>
    where
        N: CliNodeTypes<ChainSpec = C::ChainSpec>,
        L: FnOnce(WithLaunchContext<NodeBuilder<DatabaseEnv, C::ChainSpec>>, Ext) -> Fut,
        Fut: Future<Output = eyre::Result<()>>,
        CompBuilder: CliComponentsBuilder<N>,
    {
        let mut this = self.configure();
        this.set_runner(runner);
        this.run::<N, _>(
            FnLauncher::new::<C, Ext>(async move |builder, chain_spec| {
                launcher(builder, chain_spec).await
            }),
            components,
        )
    }
}

/// A wrapper around a parsed CLI that handles command execution.
#[derive(Debug)]
pub struct CliApp<Spec: ChainSpecParser, Ext: clap::Args + fmt::Debug, Rpc: RpcModuleValidator> {
    cli: Cli<Spec, Ext, Rpc>,
    runner: Option<CliRunner>,
    layers: Option<Layers>,
    guard: Option<TracingGuards>,
}

impl<C, Ext, Rpc> CliApp<C, Ext, Rpc>
where
    C: ChainSpecParser<ChainSpec = WorldChainSpec>,
    Ext: clap::Args + fmt::Debug,
    Rpc: RpcModuleValidator,
{
    pub(crate) fn new(cli: Cli<C, Ext, Rpc>) -> Self {
        Self {
            cli,
            runner: None,
            layers: Some(Layers::new()),
            guard: None,
        }
    }

    /// Sets the runner for the CLI command.
    pub fn set_runner(&mut self, runner: CliRunner) {
        self.runner = Some(runner);
    }

    /// Access to tracing layers.
    pub fn access_tracing_layers(&mut self) -> eyre::Result<&mut Layers> {
        self.layers
            .as_mut()
            .ok_or_else(|| eyre!("Tracing already initialized"))
    }

    /// Executes the configured cli command.
    pub fn run<N, CompBuilder>(
        mut self,
        launcher: impl Launcher<C, Ext>,
        components: CompBuilder,
    ) -> eyre::Result<()>
    where
        N: CliNodeTypes<ChainSpec = C::ChainSpec>,
        CompBuilder: CliComponentsBuilder<N>,
    {
        let runner = match self.runner.take() {
            Some(runner) => runner,
            None => CliRunner::try_default_runtime()?,
        };

        if let Some(chain_spec) = self.cli.command.chain_spec() {
            self.cli.logs.log_file_directory = self
                .cli
                .logs
                .log_file_directory
                .join(chain_spec.chain.to_string());
        }

        self.init_tracing(&runner)?;
        install_prometheus_recorder();

        match self.cli.command {
            Commands::Node(command) => {
                if let Some(http_api) = &command.rpc.http_api {
                    Rpc::validate_selection(http_api, "http.api").map_err(|e| eyre!("{e}"))?;
                }
                if let Some(ws_api) = &command.rpc.ws_api {
                    Rpc::validate_selection(ws_api, "ws.api").map_err(|e| eyre!("{e}"))?;
                }

                runner.run_command_until_exit(|ctx| command.execute(ctx, launcher))
            }
            Commands::Init(command) => {
                let runtime = runner.runtime();
                runner.run_blocking_until_ctrl_c(command.execute::<N>(runtime))
            }
            Commands::DumpGenesis(command) => runner.run_blocking_until_ctrl_c(command.execute()),
            Commands::Db(command) => {
                runner.run_blocking_command_until_exit(|ctx| command.execute::<N>(ctx))
            }
            Commands::Stage(command) => {
                runner.run_command_until_exit(|ctx| command.execute::<N, _>(ctx, components))
            }
            Commands::P2P(command) => runner.run_until_ctrl_c(command.execute::<N>()),
            Commands::Config(command) => runner.run_until_ctrl_c(command.execute()),
            Commands::Prune(command) => {
                runner.run_command_until_exit(|ctx| command.execute::<N>(ctx))
            }
            Commands::ReExecute(command) => {
                let runtime = runner.runtime();
                runner.run_until_ctrl_c(command.execute::<N>(components, runtime))
            }
            Commands::InitState(_)
            | Commands::ImportOp(_)
            | Commands::ImportReceiptsOp(_)
            | Commands::OpProofs(_) => Err(eyre!(
                "this OP-specific command is not yet supported with WorldChainSpec"
            )),
            #[cfg(feature = "dev")]
            Commands::TestVectors(_) => Err(eyre!(
                "test-vectors is not yet supported with WorldChainSpec"
            )),
        }
    }

    /// Initializes tracing with the configured options.
    pub fn init_tracing(&mut self, runner: &CliRunner) -> eyre::Result<()> {
        if self.guard.is_none() {
            let mut layers = self.layers.take().unwrap_or_default();

            let otlp_status = runner.block_on(self.cli.traces.init_otlp_tracing(&mut layers))?;
            let otlp_logs_status = runner.block_on(self.cli.traces.init_otlp_logs(&mut layers))?;

            let enable_reload = matches!(
                &self.cli.command,
                Commands::Node(cmd) if cmd.rpc.is_namespace_enabled(RethRpcModule::Admin)
            );

            self.guard = Some(
                self.cli
                    .logs
                    .init_tracing_with_layers(layers, enable_reload)?,
            );
            info!(target: "reth::cli", "Initialized tracing, debug log directory: {}", self.cli.logs.log_file_directory);

            if enable_reload {
                let directive = self.cli.logs.verbosity.directive().to_string();
                let baseline = if self.cli.logs.log_stdout_filter.is_empty() {
                    directive
                } else {
                    format!("{directive},{}", self.cli.logs.log_stdout_filter)
                };
                world_chain_primitives::tracing::set_startup_tracing_directives(baseline);
            }

            match otlp_status {
                OtlpInitStatus::Started(endpoint) => {
                    info!(target: "reth::cli", "Started OTLP {:?} tracing export to {endpoint}", self.cli.traces.protocol);
                }
                OtlpInitStatus::NoFeature => {
                    warn!(target: "reth::cli", "Provided OTLP tracing arguments do not have effect, compile with the `otlp` feature")
                }
                OtlpInitStatus::Disabled => {}
            }

            match otlp_logs_status {
                OtlpLogsStatus::Started(endpoint) => {
                    info!(target: "reth::cli", "Started OTLP {:?} logs export to {endpoint}", self.cli.traces.protocol);
                }
                OtlpLogsStatus::NoFeature => {
                    warn!(target: "reth::cli", "Provided OTLP logs arguments do not have effect, compile with the `otlp-logs` feature")
                }
                OtlpLogsStatus::Disabled => {}
            }
        }
        Ok(())
    }
}
