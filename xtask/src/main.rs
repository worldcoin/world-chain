use std::{fmt, fs::OpenOptions, path::PathBuf};

use clap::Parser;
use eyre::eyre::Context;
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{
    fmt::{FmtContext, FormatEvent, FormatFields, format::Writer},
    layer::SubscriberExt,
    registry::LookupSpan,
    util::SubscriberInitExt,
};

mod devnet;
mod docs;
mod preflight;
mod stress;
mod swarm;
mod toolkit;

#[derive(Parser)]
#[command(name = "xtask", about = "World Chain development tasks")]
enum Command {
    /// Generate CLI reference documentation for the mdbook
    Docs(docs::Args),
    /// Manage the native Rust World Chain devnet
    Devnet(devnet::Args),
    /// Run preflight checks (auto-fix + verify)
    Preflight(preflight::Args),
    /// Launch a local node swarm
    LaunchNode(swarm::Args),
    /// Run stress tests against a live network
    Stress(stress::Args),
    /// Prove a PBH transaction
    Prove(toolkit::Args),
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    dotenvy::dotenv().ok();

    let cmd = Command::parse();

    match cmd {
        Command::Docs(args) => {
            tracing_subscriber::fmt::init();
            docs::run(args)
        }
        Command::Devnet(args) => {
            let _log_guard = init_devnet_tracing(devnet::should_reset_log(&args))?;

            devnet::run(args).await
        }
        Command::Preflight(args) => preflight::run(args),
        Command::LaunchNode(args) => {
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .init();

            swarm::run(args).await
        }
        Command::Stress(args) => {
            tracing_subscriber::fmt::init();
            stress::run(args).await
        }
        Command::Prove(args) => toolkit::run(args).await,
    }
}

fn init_devnet_tracing(
    reset_log: bool,
) -> eyre::Result<tracing_appender::non_blocking::WorkerGuard> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let log_path = devnet_log_path();
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
    }
    if reset_log && log_path.exists() {
        std::fs::remove_file(&log_path)
            .wrap_err_with(|| format!("failed to remove old {}", log_path.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .wrap_err_with(|| format!("failed to open {}", log_path.display()))?;
    let (file_writer, guard) = tracing_appender::non_blocking(file);

    let stdout_layer = tracing_subscriber::fmt::layer()
        .event_format(DevnetLogFormatter)
        .with_ansi(true);
    let file_layer = tracing_subscriber::fmt::layer()
        .event_format(DevnetLogFormatter)
        .with_ansi(false)
        .with_writer(file_writer);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();

    tracing::info!(path = %log_path.display(), "devnet logs file");
    Ok(guard)
}

fn devnet_log_path() -> PathBuf {
    std::env::var_os("WORLD_CHAIN_DEVNET_LOG_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/devnet/logs/devnet.log"))
}

struct DevnetLogFormatter;

impl<S, N> FormatEvent<S, N> for DevnetLogFormatter
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut fields = DevnetEventFields::default();
        event.record(&mut fields);

        write_level(&mut writer, event.metadata().level())?;
        writer.write_char(' ')?;
        if let Some(process) = fields.process.as_deref() {
            write_process(&mut writer, process)?;
            writer.write_char(' ')?;
        }
        let mut wrote_message = false;
        if let Some(message) = fields.message.as_deref() {
            writer.write_str(message)?;
            wrote_message = true;
        }
        for (name, value) in fields.fields {
            if wrote_message {
                writer.write_char(' ')?;
            }
            write_field(&mut writer, &name, &value)?;
            wrote_message = true;
        }
        writer.write_char('\n')
    }
}

#[derive(Default)]
struct DevnetEventFields {
    message: Option<String>,
    process: Option<String>,
    fields: Vec<(String, String)>,
}

impl Visit for DevnetEventFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.record_value(field.name(), value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.record_value(field.name(), format!("{value:?}"));
    }
}

impl DevnetEventFields {
    fn record_value(&mut self, name: &str, value: String) {
        match name {
            "message" => self.message = Some(value),
            "process" => self.process = Some(value),
            _ => self.fields.push((name.to_string(), value)),
        }
    }
}

fn write_level(writer: &mut Writer<'_>, level: &tracing::Level) -> fmt::Result {
    let label = match *level {
        tracing::Level::ERROR => "ERROR",
        tracing::Level::WARN => "WARN ",
        tracing::Level::INFO => "INFO ",
        tracing::Level::DEBUG => "DEBUG",
        tracing::Level::TRACE => "TRACE",
    };
    if writer.has_ansi_escapes() {
        let color = match *level {
            tracing::Level::ERROR => "\x1b[31m",
            tracing::Level::WARN => "\x1b[33m",
            tracing::Level::INFO => "\x1b[32m",
            tracing::Level::DEBUG => "\x1b[34m",
            tracing::Level::TRACE => "\x1b[35m",
        };
        write!(writer, "{color}{label}\x1b[0m")
    } else {
        writer.write_str(label)
    }
}

fn write_process(writer: &mut Writer<'_>, process: &str) -> fmt::Result {
    if writer.has_ansi_escapes() {
        write!(writer, "\x1b[1;36m{process}\x1b[0m")
    } else {
        writer.write_str(process)
    }
}

fn write_field(writer: &mut Writer<'_>, name: &str, value: &str) -> fmt::Result {
    writer.write_str(name)?;
    writer.write_char('=')?;
    if value.chars().any(char::is_whitespace) {
        writer.write_char('"')?;
        for ch in value.chars() {
            if ch == '"' || ch == '\\' {
                writer.write_char('\\')?;
            }
            writer.write_char(ch)?;
        }
        writer.write_char('"')
    } else {
        writer.write_str(value)
    }
}
