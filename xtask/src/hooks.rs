//! Install git hooks for the repository.

use clap::Parser;
use eyre::{Result, eyre::eyre};
use std::path::PathBuf;
use tracing::info;

/// Install git hooks (pre-commit, etc.)
#[derive(Parser, Debug)]
pub struct Args;

pub fn run(_args: Args) -> Result<()> {
    // Use git itself to resolve the hooks path — works for repos and worktrees.
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--git-path", "hooks"])
        .output()?;

    if !output.status.success() {
        return Err(eyre!("Not in a git repository"));
    }

    let git_hooks_dir = PathBuf::from(String::from_utf8(output.stdout)?.trim());
    std::fs::create_dir_all(&git_hooks_dir)?;

    // Absolute path to the hook script — avoids relative-path fragility.
    let repo_root = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    let repo_root = PathBuf::from(String::from_utf8(repo_root.stdout)?.trim());
    let hook_script = repo_root.join("scripts/pre-commit");

    let hooks: &[(&str, &PathBuf)] = &[("pre-commit", &hook_script)];

    for (name, target) in hooks {
        let hook_path = git_hooks_dir.join(name);

        if hook_path.exists() || hook_path.is_symlink() {
            std::fs::remove_file(&hook_path)?;
            info!("Removed existing {name} hook");
        }

        #[cfg(unix)]
        std::os::unix::fs::symlink(target, &hook_path)?;

        info!("Installed {name} → {}", target.display());
    }

    info!("Git hooks installed successfully");
    Ok(())
}
