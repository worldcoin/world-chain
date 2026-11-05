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
    args::{DefaultEngineValues, LogArgs, OtlpInitStatus, OtlpLogsStatus, TraceArgs},
    version::version_metadata,
};
use reth_node_metrics::recorder::install_prometheus_recorder;
use reth_rpc_server_types::{DefaultRpcModuleValidator, RethRpcModule, RpcModuleValidator};
use reth_tracing::{Layers, TracingGuards};
use tracing::{info, warn};
use world_chain_chainspec::WorldChainSpec;

use crate::{WorldChainArgs, chainspec::WorldChainSpecParser, commands::Commands};

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

const STATE_MASKING_ARG: &str = "num_state_masking_blocks";
const STATE_MASKING_UNSUPPORTED_HELP: &str =
    "--engine.num-state-masking-blocks must be 0 on world-chain: state masking is not supported";

impl<C, Ext, Rpc> Cli<C, Ext, Rpc>
where
    C: ChainSpecParser,
    Ext: clap::Args + fmt::Debug,
    Rpc: RpcModuleValidator,
{
    /// Returns the clap command with state masking disabled and hidden, matching op-reth.
    pub fn command_with_state_masking_disabled() -> clap::Command {
        // If defaults were initialized elsewhere, the effective-value check still rejects masking.
        let _ = DefaultEngineValues::default()
            .with_num_state_masking_blocks(0)
            .try_init();
        <Self as clap::CommandFactory>::command().mut_subcommands(|cmd| {
            if cmd.get_name() == "node" {
                cmd.mut_args(|arg| {
                    if arg.get_id() == STATE_MASKING_ARG {
                        arg.hide(true)
                    } else {
                        arg
                    }
                })
            } else {
                cmd
            }
        })
    }

    /// Parses CLI arguments with state masking disabled, matching op-reth.
    pub fn parse_args() -> Self {
        match Self::try_parse_args_from(std::env::args_os()) {
            Ok(cli) => cli,
            Err(err) => err.exit(),
        }
    }

    /// Parses CLI arguments from the given iterator, rejecting state masking.
    pub fn try_parse_args_from<I, T>(itr: I) -> Result<Self, clap::error::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let mut cmd = Self::command_with_state_masking_disabled();
        let mut matches = cmd.try_get_matches_from_mut(itr)?;
        // Check the effective value, including environment variables and previously set defaults.
        if let Some(node) = matches.subcommand_matches("node")
            && node
                .get_one::<u64>(STATE_MASKING_ARG)
                .is_some_and(|blocks| *blocks != 0)
        {
            return Err(cmd.error(
                clap::error::ErrorKind::ValueValidation,
                STATE_MASKING_UNSUPPORTED_HELP,
            ));
        }
        <Self as clap::FromArgMatches>::from_arg_matches_mut(&mut matches)
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
            Commands::OpProofs(command) => {
                let runtime = runner.runtime();
                runner.run_blocking_until_ctrl_c(command.execute::<N>(runtime))
            }
        }
    }

    /// Initializes tracing with the configured options.
    pub fn init_tracing(&mut self, runner: &CliRunner) -> eyre::Result<()> {
        if self.guard.is_none() {
            let mut layers = self.layers.take().unwrap_or_default();

            let otlp_status = runner.block_on(self.cli.traces.init_otlp_tracing(&mut layers))?;
            let otlp_logs_status = runner.block_on(self.cli.traces.init_otlp_logs(&mut layers))?;

            let enable_reload = match &self.cli.command {
                Commands::Node(cmd) => {
                    let admin = &RethRpcModule::Admin;
                    (cmd.rpc.http
                        && cmd
                            .rpc
                            .http_api
                            .as_ref()
                            .is_some_and(|api| api.contains(admin)))
                        || (cmd.rpc.ws
                            && cmd
                                .rpc
                                .ws_api
                                .as_ref()
                                .is_some_and(|api| api.contains(admin)))
                }
                _ => false,
            };

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_masking_is_disabled() {
        for explicit_zero in [false, true] {
            let mut args = vec!["world-chain", "node", "--engine.persistence-threshold", "7"];
            if explicit_zero {
                args.extend(["--engine.num-state-masking-blocks", "0"]);
            }
            let cli = Cli::<WorldChainSpecParser, WorldChainArgs>::try_parse_args_from(args)
                .expect("masking disabled");
            let Commands::Node(command) = cli.command else {
                panic!("expected node command");
            };
            assert_eq!(command.engine.num_state_masking_blocks(), 0);
            command
                .engine
                .validate()
                .expect("legacy persistence threshold");
        }

        for value in ["1", "30"] {
            let err = Cli::<WorldChainSpecParser, WorldChainArgs>::try_parse_args_from([
                "world-chain",
                "node",
                "--engine.num-state-masking-blocks",
                value,
            ])
            .expect_err("nonzero masking must be rejected");
            assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
            assert!(err.to_string().contains(STATE_MASKING_UNSUPPORTED_HELP));
        }

        let mut cmd =
            Cli::<WorldChainSpecParser, WorldChainArgs>::command_with_state_masking_disabled();
        let help = cmd
            .find_subcommand_mut("node")
            .expect("node subcommand")
            .render_long_help()
            .to_string();
        assert!(!help.contains("--engine.num-state-masking-blocks"));
        assert!(help.contains("--engine.persistence-threshold"));
    }
}
