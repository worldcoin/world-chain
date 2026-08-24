//! Releases, using git tags as the log.
//!
//! There is no separate append-only release file. A release *is* the tag `proofs/vX.Y.Z`, and
//! what it shipped is `measurements.toml` at that tag:
//!
//! ```text
//! git show proofs/v1.0.0:measurements.toml
//! ```
//!
//! That removes a whole class of bug: a second file recording the same measurements can
//! disagree with the lock, and then something has to decide which one is authoritative. A tag
//! cannot disagree with the tree it points at.

use std::{path::Path, process::Command, str::FromStr};

use eyre::eyre::{bail, eyre};

use crate::{
    capture::{capture, run_cmd},
    document::PATH,
    require_clean_tree,
};

/// Prefix for release tags.
const TAG_PREFIX: &str = "proofs/v";

/// A released version: `X.Y.Z`, optionally an `-rc.N` prerelease of it.
///
/// Only these two shapes are accepted. A version we cannot order exactly is one we must not
/// mint, because ordering is how "which release is newer" gets answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    rc: Option<u64>,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(rc) = self.rc {
            write!(f, "-rc.{rc}")?;
        }
        Ok(())
    }
}

impl FromStr for Version {
    type Err = eyre::Report;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (triple, rc) = match s.split_once("-rc.") {
            Some((triple, n)) => (
                triple,
                Some(
                    n.parse()
                        .map_err(|_| eyre!("version {s:?}: rc number must be an integer"))?,
                ),
            ),
            None => {
                if s.contains('-') || s.contains('+') {
                    bail!("version {s:?}: only `X.Y.Z` and `X.Y.Z-rc.N` are supported");
                }
                (s, None)
            }
        };
        let parts: Vec<&str> = triple.split('.').collect();
        let [major, minor, patch] = parts.as_slice() else {
            bail!("version {s:?}: expected three dot-separated components");
        };
        let parse = |part: &str, name: &str| -> eyre::Result<u64> {
            if part.is_empty() || (part.len() > 1 && part.starts_with('0')) {
                bail!("version {s:?}: {name} must not be empty or zero-padded");
            }
            part.parse()
                .map_err(|_| eyre!("version {s:?}: {name} must be an integer"))
        };
        Ok(Self {
            major: parse(major, "major")?,
            minor: parse(minor, "minor")?,
            patch: parse(patch, "patch")?,
            rc,
        })
    }
}

/// Tags the current commit as `proofs/vX.Y.Z`, refusing unless the measurements are current.
///
/// The order matters: the tree must be clean before the rebuild, so what is measured is what
/// gets tagged, and the rebuild must pass before the tag exists, so no tag can ever name
/// measurements nobody reproduced.
pub fn run(root: &Path, version: &str, dry_run: bool) -> eyre::Result<()> {
    let version: Version = version.parse()?;
    let tag = format!("{TAG_PREFIX}{version}");

    if tag_exists(&tag)? {
        bail!("{tag} already exists; releases are immutable");
    }
    require_clean_tree()?;

    println!("rebuilding measurements to confirm {PATH} is current...");
    crate::capture::run(root, true)?;

    let commit = capture(
        Command::new("git").args(["rev-parse", "HEAD"]),
        "resolving HEAD",
    )?
    .trim()
    .to_string();

    if dry_run {
        println!("would tag {tag} at {commit}");
        return Ok(());
    }

    run_cmd(
        Command::new("git")
            .args(["tag", "-a", &tag, "-m"])
            .arg(format!("proof release {version}")),
        "creating the release tag",
    )?;
    println!(
        "tagged {tag} at {commit}\n\
         Push it to release: git push origin {tag}\n\
         What it shipped:    git show {tag}:{PATH}"
    );
    Ok(())
}

fn tag_exists(tag: &str) -> eyre::Result<bool> {
    let out = capture(
        Command::new("git").args(["tag", "--list", tag]),
        "listing tags",
    )?;
    Ok(!out.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn parses_and_displays_both_supported_shapes() {
        for s in ["1.0.0", "1.0.0-rc.1", "0.0.0", "10.20.30-rc.44"] {
            assert_eq!(v(s).to_string(), s);
        }
    }

    #[test]
    fn rejects_shapes_we_cannot_order() {
        for s in [
            "1.0",
            "1.0.0.0",
            "1.0.0-beta.1",
            "1.0.0-rc",
            "1.0.0-rc.x",
            "v1.0.0",
            "1.0.0+build",
            "01.0.0",
            "",
        ] {
            assert!(s.parse::<Version>().is_err(), "should reject {s:?}");
        }
    }

    /// The tag is the log entry, so its spelling is load-bearing: `git show
    /// proofs/v1.0.0:measurements.toml` has to work for the exact string we print.
    #[test]
    fn tag_spelling_is_stable() {
        assert_eq!(
            format!("{TAG_PREFIX}{}", v("1.2.3-rc.4")),
            "proofs/v1.2.3-rc.4"
        );
    }
}
