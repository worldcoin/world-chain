use clap::Parser;

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
