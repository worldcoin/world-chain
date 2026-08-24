//! Keeps the measured workspaces' dependency tables from drifting apart.
//!
//! Every measured crate is its own workspace with its own `[workspace.dependencies]`, which
//! is what stops the node's dependency graph from moving a vkey or a PCR. The cost is four
//! copies of the same pins and nothing tying them together — and they build each other:
//! `proofs/core` is compiled inside the enclave's resolve and inside the SP1 guests' resolve.
//! If two of those tables name the same crate at different versions or from different
//! sources, cargo resolves them as unrelated packages and the build fails somewhere far from
//! the cause. This check is the thing a shared workspace would have given for free.

use std::{collections::BTreeMap, path::Path};

use eyre::eyre::{Context, bail};

/// The measured workspaces, relative to the repo root.
///
/// The root workspace is deliberately absent: it is allowed to differ, and the whole point of
/// the split is that it does not constrain these.
pub const MEASURED: &[(&str, &str)] = &[
    ("core", "proofs/core"),
    ("kona-client", "proofs/kona/client"),
    ("enclave", "proofs/backends/nitro/enclave"),
    ("sp1-programs", "proofs/backends/sp1/programs"),
];

/// What a dependency resolves to, reduced to the parts that decide package identity.
///
/// Features are excluded on purpose. Each workspace is a separate resolve, so differing
/// features produce differently-configured builds but never two incompatible copies of one
/// crate. A differing version or source does exactly that.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Identity {
    Registry {
        version: String,
    },
    Git {
        url: String,
        reference: String,
    },
    /// Path deps are compared by their location relative to the repo root, since the same
    /// crate is reached by a different number of `..` from each workspace.
    Path {
        resolved: String,
    },
}

impl Identity {
    fn describe(&self) -> String {
        match self {
            Self::Registry { version } => format!("registry {version}"),
            Self::Git { url, reference } => format!("git {url} @ {reference}"),
            Self::Path { resolved } => format!("path {resolved}"),
        }
    }
}

fn identity(workspace_dir: &str, spec: &toml::Value) -> eyre::Result<Identity> {
    if let Some(version) = spec.as_str() {
        return Ok(Identity::Registry {
            version: version.to_string(),
        });
    }
    let table = spec
        .as_table()
        .ok_or_else(|| eyre::eyre::eyre!("dependency spec is neither a string nor a table"))?;

    if let Some(git) = table.get("git").and_then(|v| v.as_str()) {
        // A git dep without a tag/rev/branch floats, which no measured crate may do.
        let reference = ["tag", "rev", "branch"]
            .iter()
            .find_map(|k| table.get(*k).and_then(|v| v.as_str()))
            .ok_or_else(|| {
                eyre::eyre::eyre!("git dependency on {git} pins no tag, rev or branch")
            })?;
        return Ok(Identity::Git {
            url: git.to_string(),
            reference: reference.to_string(),
        });
    }

    if let Some(path) = table.get("path").and_then(|v| v.as_str()) {
        return Ok(Identity::Path {
            resolved: normalise(workspace_dir, path),
        });
    }

    let version = table
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| eyre::eyre::eyre!("dependency has no version, git or path"))?;
    Ok(Identity::Registry {
        version: version.to_string(),
    })
}

/// Collapses `proofs/backends/nitro/enclave` + `../../../core` to `proofs/core`, so the same
/// crate compares equal however it was reached.
fn normalise(workspace_dir: &str, relative: &str) -> String {
    let mut parts: Vec<&str> = workspace_dir.split('/').filter(|p| !p.is_empty()).collect();
    for component in relative.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// Parses `[workspace.dependencies]` out of one manifest.
fn workspace_dependencies(root: &Path, dir: &str) -> eyre::Result<BTreeMap<String, toml::Value>> {
    let path = root.join(dir).join("Cargo.toml");
    let text =
        std::fs::read_to_string(&path).wrap_err_with(|| format!("reading {}", path.display()))?;
    let doc: toml::Value =
        toml::from_str(&text).wrap_err_with(|| format!("parsing {}", path.display()))?;
    let table = doc
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(|d| d.as_table())
        .ok_or_else(|| {
            eyre::eyre::eyre!(
                "{} has no [workspace.dependencies]; every measured crate must pin its own",
                path.display()
            )
        })?;
    Ok(table.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

/// Fails if any crate is pinned differently by two measured workspaces.
pub fn check(root: &Path) -> eyre::Result<()> {
    let mut tables = Vec::new();
    for (name, dir) in MEASURED {
        tables.push((*name, *dir, workspace_dependencies(root, dir)?));
    }

    // crate name -> workspace -> identity
    let mut by_crate: BTreeMap<String, Vec<(&str, Identity)>> = BTreeMap::new();
    for (name, dir, deps) in &tables {
        for (crate_name, spec) in deps {
            let id = identity(dir, spec)
                .wrap_err_with(|| format!("{dir}: [workspace.dependencies] {crate_name}"))?;
            by_crate
                .entry(crate_name.clone())
                .or_default()
                .push((name, id));
        }
    }

    let mut conflicts = Vec::new();
    for (crate_name, holders) in &by_crate {
        let first = &holders[0].1;
        if holders.iter().any(|(_, id)| id != first) {
            let detail = holders
                .iter()
                .map(|(ws, id)| format!("    {ws}: {}", id.describe()))
                .collect::<Vec<_>>()
                .join("\n");
            conflicts.push(format!("  {crate_name}\n{detail}"));
        }
    }

    if !conflicts.is_empty() {
        bail!(
            "measured workspaces disagree on {} dependenc{}:\n{}\n\n\
             These tables must agree: the crates they pin are compiled into each other, so a \
             version or source difference resolves as two unrelated packages.\n\
             Changing a pin here changes a vkey or a PCR — align them deliberately.",
            conflicts.len(),
            if conflicts.len() == 1 { "y" } else { "ies" },
            conflicts.join("\n")
        );
    }

    let shared = by_crate.values().filter(|h| h.len() > 1).count();
    println!(
        "measured workspaces agree: {} crate(s) pinned in more than one of {}",
        shared,
        MEASURED.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses one dependency spec the way it appears in a manifest.
    fn spec(s: &str) -> toml::Value {
        toml::from_str::<toml::Value>(&format!("d = {s}")).unwrap()["d"].clone()
    }

    #[test]
    fn normalises_paths_to_the_repo_root() {
        assert_eq!(
            normalise("proofs/backends/nitro/enclave", "../../../core"),
            "proofs/core"
        );
        assert_eq!(normalise("proofs/kona/client", "../../core"), "proofs/core");
        assert_eq!(
            normalise("proofs/backends/sp1/programs", "range-utils"),
            "proofs/backends/sp1/programs/range-utils"
        );
    }

    /// The same crate reached from two different workspaces must compare equal, or the check
    /// reports a conflict on every shared path dependency and is useless.
    #[test]
    fn path_deps_from_different_depths_are_the_same_identity() {
        let from_client =
            identity("proofs/kona/client", &spec(r#"{ path = "../../core" }"#)).unwrap();
        let from_enclave = identity(
            "proofs/backends/nitro/enclave",
            &spec(r#"{ path = "../../../core" }"#),
        )
        .unwrap();
        assert_eq!(from_client, from_enclave);
    }

    #[test]
    fn version_difference_is_a_conflict() {
        assert_ne!(
            identity("x", &spec(r#""=2.1.1""#)).unwrap(),
            identity("x", &spec(r#""2.0.5""#)).unwrap()
        );
    }

    /// Features differ legitimately between workspaces — each is a separate resolve — so they
    /// must not be part of the identity or the check cries wolf.
    #[test]
    fn features_do_not_affect_identity() {
        assert_eq!(
            identity("x", &spec(r#"{ version = "1.6.0" }"#)).unwrap(),
            identity(
                "x",
                &spec(r#"{ version = "1.6.0", features = ["serde"], default-features = false }"#)
            )
            .unwrap()
        );
    }

    #[test]
    fn git_reference_is_part_of_identity() {
        assert_ne!(
            identity("x", &spec(r#"{ git = "https://e.x/o", tag = "v1" }"#)).unwrap(),
            identity("x", &spec(r#"{ git = "https://e.x/o", tag = "v2" }"#)).unwrap()
        );
    }

    /// A measured crate must never track a moving git reference.
    #[test]
    fn rejects_a_floating_git_dependency() {
        let err = identity("x", &spec(r#"{ git = "https://e.x/o" }"#)).unwrap_err();
        assert!(err.to_string().contains("pins no tag"), "{err}");
    }

    /// The real manifests must agree. This is the check itself, run as a test, so a drifting
    /// pin fails `cargo test` and not only the dedicated CI step.
    #[test]
    fn the_repository_is_consistent() {
        let root = crate::capture::repo_root().expect("git repo");
        check(&root).expect("measured workspaces must pin shared dependencies identically");
    }
}
