use std::{fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;
use world_chain_proof_sp1_host::vkeys::{EmbeddedVkeyManifest, embedded_vkey_manifest};

#[derive(Debug, Args)]
pub struct VkeysArgs {
    /// Fail unless the embedded ELF hashes and vkeys match this manifest.
    #[arg(long)]
    check: Option<PathBuf>,
}

pub async fn vkeys(args: VkeysArgs) -> Result<()> {
    let actual = embedded_vkey_manifest().await?;
    if let Some(path) = args.check {
        let expected: EmbeddedVkeyManifest = serde_json::from_slice(
            &fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?,
        )
        .with_context(|| format!("failed to parse {}", path.display()))?;
        if actual != expected {
            bail!(
                "embedded SP1 measurements do not match {}\nexpected: {expected:#?}\nactual: {actual:#?}",
                path.display()
            );
        }
    }

    println!("{}", serde_json::to_string_pretty(&actual)?);
    Ok(())
}
