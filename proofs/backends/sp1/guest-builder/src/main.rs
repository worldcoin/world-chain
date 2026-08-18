use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result, ensure};
use sp1_build::{BuildArgs, execute_build_program};

fn main() -> Result<()> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .context("guest builder must live at proofs/backends/sp1/guest-builder")?
        .to_path_buf();
    let programs_root = repo_root.join("proofs/backends/sp1/programs");
    let target_root = programs_root.join("target/elf-compilation");

    for program in ["range-ethereum", "aggregation"] {
        let outputs = execute_build_program(
            &BuildArgs {
                ignore_rust_version: true,
                locked: true,
                ..Default::default()
            },
            Some(programs_root.join(program)),
        )
        .with_context(|| format!("failed to build SP1 guest {program}"))?;

        ensure!(
            outputs.len() == 1,
            "SP1 guest {program} must produce exactly one ELF"
        );
        for (_, source) in outputs {
            let relative = source.strip_prefix(&target_root).with_context(|| {
                format!(
                    "SP1 guest output {} is outside {}",
                    source,
                    target_root.display()
                )
            })?;
            let destination = target_root.join("docker").join(relative);
            fs::create_dir_all(
                destination
                    .parent()
                    .context("SP1 guest destination must have a parent")?,
            )?;
            fs::copy(&source, &destination).with_context(|| {
                format!("failed to copy {} to {}", source, destination.display())
            })?;
        }
    }

    Ok(())
}
