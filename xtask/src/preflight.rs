//! Pre-commit preflight checks — auto-fix and verify with maximum parallelism.

use clap::Parser;
use eyre::Result;
use std::{
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

/// Run all preflight checks (formatting, clippy, docs, solidity).
#[derive(Parser, Debug)]
pub struct Args;

const RED: &str = "\x1b[0;31m";
const GREEN: &str = "\x1b[0;32m";
const YELLOW: &str = "\x1b[0;33m";
const GRAY: &str = "\x1b[0;90m";
const NC: &str = "\x1b[0m";

enum Status {
    Pass,
    Fixed,
    Fail(String),
    FailWith(String, String),
    Skip,
}

fn staged_paths_match(prefix: &str) -> bool {
    Command::new("git")
        .args(["diff", "--cached", "--name-only"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.starts_with(prefix))
        })
        .unwrap_or(false)
}

/// Run a command, suppress all output. Returns success.
fn run_cmd(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run a command, capture stderr. Returns (success, stderr).
fn run_cmd_capture(cmd: &str, args: &[&str]) -> (bool, String) {
    match Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
    {
        Ok(output) => (
            output.status.success(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ),
        Err(e) => (false, format!("failed to run {cmd}: {e}")),
    }
}

/// Run a command with an env var, capture stderr.
fn run_cmd_capture_env(cmd: &str, args: &[&str], key: &str, val: &str) -> (bool, String) {
    match Command::new(cmd)
        .args(args)
        .env(key, val)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
    {
        Ok(output) => (
            output.status.success(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ),
        Err(e) => (false, format!("failed to run {cmd}: {e}")),
    }
}

/// Extract only warning/error lines from compiler output, deduplicating
/// the "N warnings generated" noise.
fn condensed_diagnostics(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|line| {
            line.starts_with("warning")
                || line.starts_with("error")
                || line.contains("-->")
                || line.starts_with("Diff in")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn restage_changed() {
    if let Ok(output) = Command::new("git").args(["diff", "--name-only"]).output() {
        let files: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(String::from)
            .collect();
        if !files.is_empty() {
            let _ = Command::new("git").arg("add").args(&files).status();
        }
    }
}

pub fn run(_args: Args) -> Result<()> {
    eprintln!("Running preflight checks...\n");

    let contracts_changed = staged_paths_match("pkg/contracts/");
    let cli_changed = staged_paths_match("crates/cli/");
    let errors = AtomicUsize::new(0);

    // ── Phase 1: Auto-fix ──────────────────────────────────────────────
    // cargo fmt and forge fmt don't contend — run them in parallel.
    // clippy --fix needs the cargo lock, so it runs after fmt.

    std::thread::scope(|s| {
        s.spawn(|| {
            run_cmd("cargo", &["+nightly", "fmt", "--all"]);
        });

        if contracts_changed {
            s.spawn(|| {
                run_cmd("forge", &["fmt", "--root", "pkg/contracts"]);
            });
        }
    });

    // Re-stage everything that was auto-fixed
    restage_changed();

    // ── Phase 2: Verify ────────────────────────────────────────────────
    // All checks are read-only. cargo fmt --check and clippy both use the
    // build cache and don't write artifacts, so they can run concurrently.
    // Non-cargo tools (forge, git diff) run in parallel too.

    std::thread::scope(|s| {
        // Rust fmt check
        s.spawn(|| {
            let (ok, stderr) =
                run_cmd_capture("cargo", &["+nightly", "fmt", "--all", "--", "--check"]);
            if ok {
                report("Rust formatting", Status::Pass, &errors);
            } else {
                let diag = condensed_diagnostics(&stderr);
                report(
                    "Rust formatting",
                    Status::FailWith("cargo +nightly fmt --all".into(), diag),
                    &errors,
                );
            }
        });

        // Clippy check
        s.spawn(|| {
            let (ok, stderr) = run_cmd_capture_env(
                "cargo",
                &[
                    "+nightly",
                    "clippy",
                    "--workspace",
                    "--all-targets",
                    "--all-features",
                    "--quiet",
                ],
                "RUSTFLAGS",
                "-D warnings",
            );
            if ok {
                report("Clippy", Status::Pass, &errors);
            } else {
                let diag = condensed_diagnostics(&stderr);
                report(
                    "Clippy",
                    Status::FailWith(
                        "cargo +nightly clippy --workspace --all-targets --all-features".into(),
                        diag,
                    ),
                    &errors,
                );
            }
        });

        // CLI docs — only if world-chain-cli changed
        s.spawn(|| {
            if !cli_changed {
                report("CLI docs", Status::Skip, &errors);
                return;
            }
            let _ = run_cmd("cargo", &["xtask", "docs"]);
            let docs_ok = Command::new("git")
                .args(["diff", "--quiet", "specs/"])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if docs_ok {
                report("CLI docs", Status::Fixed, &errors);
            } else {
                let _ = Command::new("git")
                    .args(["checkout", "--", "specs/"])
                    .status();
                report("CLI docs", Status::Fail("cargo xtask docs".into()), &errors);
            }
        });

        // Solidity fmt check — only if contracts/ changed
        s.spawn(|| {
            if !contracts_changed {
                report("Solidity formatting", Status::Skip, &errors);
                return;
            }
            let ok = run_cmd("forge", &["fmt", "--check", "--root", "pkg/contracts"]);
            report(
                "Solidity formatting",
                if ok {
                    Status::Pass
                } else {
                    Status::Fail("cd pkg/contracts && forge fmt".into())
                },
                &errors,
            );
        });
    });

    eprintln!();
    let total_errors = errors.load(Ordering::SeqCst);
    if total_errors > 0 {
        eprintln!("{RED}{total_errors} check(s) failed.{NC}");
        std::process::exit(1);
    }
    eprintln!("{GREEN}All preflight checks passed.{NC}");
    Ok(())
}

fn report(name: &str, status: Status, errors: &AtomicUsize) {
    match status {
        Status::Pass => eprintln!("{GREEN}PASS{NC} {name}"),
        Status::Fixed => eprintln!("{YELLOW}FIX {NC} {name} (auto-fixed & staged)"),
        Status::Skip => eprintln!("{GRAY}SKIP{NC} {name}"),
        Status::Fail(hint) => {
            eprintln!("{RED}FAIL{NC} {name} — run '{hint}'");
            errors.fetch_add(1, Ordering::SeqCst);
        }
        Status::FailWith(hint, diagnostics) => {
            eprintln!("{RED}FAIL{NC} {name} — run '{hint}'");
            if !diagnostics.is_empty() {
                for line in diagnostics.lines() {
                    eprintln!("      {GRAY}{line}{NC}");
                }
            }
            errors.fetch_add(1, Ordering::SeqCst);
        }
    }
}
