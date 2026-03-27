//! Auto-generate CLI documentation for the mdbook.
//!
//! Renders `--help` output from each clap-derived CLI struct into markdown
//! and writes them to `specs/cli/`. The generated pages are included in
//! `SUMMARY.md` so `mdbook build` picks them up automatically.

use clap::{CommandFactory, Parser};
use eyre::Result;
use std::path::PathBuf;
use tracing::info;

/// Generate CLI reference documentation for the mdbook.
#[derive(Parser, Debug)]
pub struct Args {
    /// Output directory for generated markdown (relative to repo root).
    #[arg(long, default_value = "specs/cli")]
    pub output: PathBuf,
}

pub fn run(args: Args) -> Result<()> {
    let out = &args.output;
    std::fs::create_dir_all(out)?;

    // Generate the world-chain node help
    let node_help = WorldChainCli::command().render_long_help().to_string();

    // Write the main CLI reference page
    let mut content = String::from(
        "# CLI Reference\n\n\
         > **Auto-generated** — run `cargo xtask docs` to regenerate.\n\n\
         ## `world-chain`\n\n\
         The World Chain node binary. All flags below are passed to `world-chain`.\n\n\
         ```text\n",
    );
    content.push_str(&node_help);
    content.push_str("\n```\n");

    let cli_md = out.join("reference.md");
    std::fs::write(&cli_md, &content)?;
    info!(?cli_md, "Wrote CLI reference");

    // Update SUMMARY.md if the CLI section isn't already there
    let summary_path = PathBuf::from("specs/SUMMARY.md");
    if summary_path.exists() {
        let summary = std::fs::read_to_string(&summary_path)?;
        if !summary.contains("cli/reference.md") {
            let updated =
                summary.trim_end().to_string() + "\n- [CLI Reference](./cli/reference.md)\n";
            std::fs::write(&summary_path, updated)?;
            info!("Updated SUMMARY.md with CLI reference link");
        }
    }

    info!("CLI documentation generated successfully");
    Ok(())
}

/// Top-level CLI for `world-chain` — mirrors the binary's arg structure
/// so we can call `CommandFactory::command()` without running the node.
#[derive(Parser)]
#[command(name = "world-chain", about = "World Chain Node")]
struct WorldChainCli {
    #[command(flatten)]
    world_chain: cli::WorldChainArgs,
}
