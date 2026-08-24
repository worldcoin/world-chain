//! Proof measurements: what the system commits to, and how a release records it.
//!
//! Deliberately not part of `xtask`. `xtask` reaches the prover through `world-chain-devnet`,
//! so building it compiles the SP1 guests — which would make `check-manifests`, a pure TOML
//! comparison, as expensive as a full guest build.

use std::process::Command;

use clap::Parser;
use eyre::eyre::bail;

mod capture;
mod document;
mod manifests;
mod release;

#[derive(Parser)]
#[command(name = "measurements", about = "Proof measurements and releases")]
enum Cmd {
    /// Fail if the measured workspaces pin a shared dependency differently
    CheckManifests,
    /// Rebuild every measurement from source and write measurements.toml
    Measure {
        /// Compare against the committed file instead of writing it. Used by CI.
        #[arg(long)]
        check: bool,
    },
    /// Print measurements.toml as flat JSON
    Show,
    /// Tag the current commit as a release, after proving its measurements are current
    Release {
        /// Version to release, e.g. 1.0.0 or 1.0.0-rc.1
        version: String,
        /// Print what would happen without creating the tag
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> eyre::Result<()> {
    let root = capture::repo_root()?;
    match Cmd::parse() {
        Cmd::CheckManifests => manifests::check(&root),
        Cmd::Measure { check } => capture::run(&root, check),
        Cmd::Show => {
            let text = std::fs::read_to_string(root.join(document::PATH))?;
            let doc = document::Measurements::parse(&text)?;
            println!("{}", serde_json::to_string_pretty(&doc.to_json())?);
            Ok(())
        }
        Cmd::Release { version, dry_run } => release::run(&root, &version, dry_run),
    }
}

/// Refuses to act on a dirty tree.
///
/// A release names a commit, and the commit only identifies what was measured if there are no
/// local edits.
pub fn require_clean_tree() -> eyre::Result<()> {
    let status = capture::capture(
        Command::new("git").args(["status", "--porcelain"]),
        "checking the working tree",
    )?;
    if !status.trim().is_empty() {
        bail!(
            "refusing to release with a dirty working tree:\n{status}\n\
             Commit or stash first so the tag identifies the measured source."
        );
    }
    Ok(())
}
