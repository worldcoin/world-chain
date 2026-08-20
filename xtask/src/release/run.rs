//! Promotes the locked measurements into the append-only release log.

use std::{path::PathBuf, process::Command};

use eyre::eyre::{Context, bail, eyre};

use crate::{
    measure::lock::{LOCK_PATH, Lock},
    release::{
        log::{LOG_PATH, Log},
        version::Bump,
    },
};

/// `cargo xtask release`
#[derive(Debug, clap::Args)]
pub struct Args {
    /// Cut the next release candidate: continues an open rc series, or opens one.
    #[arg(long, group = "what")]
    rc: bool,
    /// Cut the next patch release.
    #[arg(long, group = "what")]
    patch: bool,
    /// Cut the next minor release.
    #[arg(long, group = "what")]
    minor: bool,
    /// Cut the next major release.
    #[arg(long, group = "what")]
    major: bool,
    /// Promote the open release candidate to stable, reusing its measurements unchanged.
    #[arg(long, group = "what")]
    promote: bool,
    /// Verify the log without writing it. Used by CI.
    #[arg(long)]
    check: bool,
    /// Prior version of the log to enforce append-only against (CI passes the base branch's).
    #[arg(long)]
    base: Option<PathBuf>,
}

pub fn run(args: Args) -> eyre::Result<()> {
    let root = repo_root()?;
    let log_path = root.join(LOG_PATH);
    let text = std::fs::read_to_string(&log_path).unwrap_or_default();
    let mut log = if text.trim().is_empty() {
        Log::default()
    } else {
        Log::parse(&text)?
    };

    if let Some(base) = &args.base {
        let base_text = std::fs::read_to_string(base)
            .wrap_err_with(|| format!("reading {}", base.display()))?;
        log.check_append_only(&Log::parse(&base_text)?)?;
    }

    if args.check {
        log.validate()?;
        println!("{LOG_PATH} is valid");
        return Ok(());
    }

    // A release must correspond to a real, reproducible build, so refuse to promote
    // measurements that no longer describe the tree they claim to come from.
    let lock_text = std::fs::read_to_string(root.join(LOCK_PATH)).wrap_err_with(|| {
        format!("cannot read {LOCK_PATH}; run `cargo xtask measure` before releasing")
    })?;
    let lock = Lock::parse(&lock_text)?;
    require_clean_tree()?;
    let commit = head_commit()?;

    let version = if args.promote {
        log.promote()?
    } else {
        let bump = if args.rc {
            Bump::Rc
        } else if args.patch {
            Bump::Patch
        } else if args.minor {
            Bump::Minor
        } else if args.major {
            Bump::Major
        } else {
            bail!("pick one of --rc, --patch, --minor, --major, or --promote");
        };
        log.cut(bump, &commit, &lock)?
    };

    std::fs::write(&log_path, log.render()?)
        .wrap_err_with(|| format!("writing {}", log_path.display()))?;
    println!("cut {version}; commit the updated {LOG_PATH}");
    Ok(())
}

/// Refuses to release from a dirty tree: the `commit` recorded in the entry has to identify
/// the source the measurements were built from, and it cannot if there are local edits.
fn require_clean_tree() -> eyre::Result<()> {
    let status = capture(Command::new("git").args(["status", "--porcelain"]))?;
    if !status.trim().is_empty() {
        bail!(
            "refusing to release with a dirty working tree:\n{status}\n\
             Commit or stash first so the recorded commit identifies the measured source."
        );
    }
    Ok(())
}

fn head_commit() -> eyre::Result<String> {
    Ok(capture(Command::new("git").args(["rev-parse", "HEAD"]))?
        .trim()
        .to_string())
}

fn repo_root() -> eyre::Result<PathBuf> {
    Ok(PathBuf::from(
        capture(Command::new("git").args(["rev-parse", "--show-toplevel"]))?.trim(),
    ))
}

fn capture(cmd: &mut Command) -> eyre::Result<String> {
    let output = cmd
        .output()
        .wrap_err_with(|| format!("failed to spawn {:?}", cmd.get_program()))?;
    if !output.status.success() {
        bail!(
            "{:?} exited with {}\n{}",
            cmd.get_program(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).map_err(|e| eyre!("stdout is not UTF-8: {e}"))
}
