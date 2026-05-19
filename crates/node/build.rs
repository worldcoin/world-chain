use std::{env, error::Error};

use vergen::{BuildBuilder, CargoBuilder, Emitter};
use vergen_git2::Git2Builder;

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-env-changed=VERGEN_GIT_SHA");

    let supplied_sha = env::var("VERGEN_GIT_SHA")
        .ok()
        .filter(|sha| !sha.trim().is_empty());
    let mut emitter = Emitter::default();

    emitter.add_instructions(&BuildBuilder::default().build_timestamp(true).build()?)?;
    emitter.add_instructions(
        &CargoBuilder::default()
            .features(true)
            .target_triple(true)
            .build()?,
    )?;
    if supplied_sha.is_none() {
        emitter.add_instructions(
            &Git2Builder::default()
                .describe(false, true, None)
                .dirty(true)
                .sha(true)
                .build()?,
        )?;
    }

    emitter.emit_and_set()?;

    let sha = env::var("VERGEN_GIT_SHA")?;
    let short_sha = sha
        .get(..7)
        .ok_or("VERGEN_GIT_SHA must be at least 7 characters")?;
    let pkg_version = env!("CARGO_PKG_VERSION");
    let target = env::var("VERGEN_CARGO_TARGET_TRIPLE")?;
    let profile = build_profile()?;
    let client_version = format!("world-chain/v{pkg_version}-{short_sha}/{target}");

    println!("cargo:rustc-env=VERGEN_GIT_SHA={sha}");
    println!("cargo:rustc-env=VERGEN_GIT_SHA_SHORT={short_sha}");
    println!("cargo:rustc-env=WORLD_CHAIN_CLIENT_VERSION={client_version}");
    println!("cargo:rustc-env=RETH_BUILD_PROFILE={profile}");
    println!("cargo:rustc-env=RETH_SHORT_VERSION={pkg_version} ({short_sha})");
    println!("cargo:rustc-env=RETH_LONG_VERSION_0=Version: {pkg_version}");
    println!("cargo:rustc-env=RETH_LONG_VERSION_1=Commit SHA: {sha}");
    println!(
        "cargo:rustc-env=RETH_LONG_VERSION_2=Build Timestamp: {}",
        env::var("VERGEN_BUILD_TIMESTAMP")?
    );
    println!(
        "cargo:rustc-env=RETH_LONG_VERSION_3=Build Features: {}",
        env::var("VERGEN_CARGO_FEATURES")?
    );
    println!("cargo:rustc-env=RETH_LONG_VERSION_4=Build Profile: {profile}");
    println!("cargo:rustc-env=RETH_P2P_CLIENT_VERSION={client_version}");

    Ok(())
}

fn build_profile() -> Result<String, Box<dyn Error>> {
    let out_dir = env::var("OUT_DIR")?;
    Ok(out_dir
        .rsplit(std::path::MAIN_SEPARATOR)
        .nth(3)
        .ok_or("OUT_DIR did not include a Cargo profile")?
        .to_string())
}
