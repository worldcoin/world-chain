use clap::Parser;

mod playground;
mod stress;
mod toolkit;

#[derive(Parser)]
#[command(name = "xtask", about = "World Chain development tasks")]
enum Command {
    /// Launch a local node playground
    Playground(playground::Args),
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
        Command::Playground(args) => {
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .init();

            playground::run(args).await
        }
        Command::Stress(args) => {
            tracing_subscriber::fmt::init();
            stress::run(args).await
        }
        Command::Prove(args) => toolkit::run(args).await,
    }
}
