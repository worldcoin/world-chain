//! Build script: compile the World Chain SP1 guest programs and emit
//! `SP1_ELF_<crate>` environment variables for `sp1_sdk::include_elf!()`.
//!
//! This is the OP Succinct upstream pattern (see `utils/build/` in
//! `succinctlabs/op-succinct`): the ELF bytes live entirely as compile-time
//! build artifacts, embedded into the host binary at link time via
//! `include_elf!()`. There are no committed ELF blobs and no runtime
//! `fs::read` of an ELF file.
//!
//! Behaviour:
//! - Only runs when the parent crate's `sp1` feature is enabled
//!   (`CARGO_FEATURE_SP1` is set by Cargo). Builds without the feature
//!   skip the SP1 compile entirely.
//! - By default uses `docker: true` with the pinned SP1 toolchain tag
//!   (matches the `=6.1.0` version of `sp1-sdk` / `sp1-zkvm` the workspace
//!   pins to) for bit-for-bit reproducible ELFs. Set
//!   `SP1_BUILD_DOCKER=false` to use a locally-installed `cargo-prove` /
//!   `sp1up` toolchain instead — useful inside container builds where the
//!   Docker daemon isn't reachable.
//! - Honours `SP1_SKIP_PROGRAM_BUILD=true` for fast iteration: when set, the
//!   build is skipped but the `SP1_ELF_*` env vars are still emitted so
//!   `include_elf!()` resolves against a previously-built ELF in
//!   `target/elf-compilation/...`. Useful for `cargo check`/`clippy` once
//!   a single full build has populated the target directory.

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_SP1");
    println!("cargo:rerun-if-env-changed=SP1_SKIP_PROGRAM_BUILD");
    println!("cargo:rerun-if-env-changed=SP1_BUILD_DOCKER");

    // Non-sp1 builds (witness generation only, nitro-only, etc.) don't need
    // the guest ELFs and skipping here avoids forcing every consumer to
    // install Docker + the SP1 toolchain.
    if std::env::var_os("CARGO_FEATURE_SP1").is_none() {
        return;
    }

    let docker = std::env::var("SP1_BUILD_DOCKER")
        .map(|v| !matches!(v.as_str(), "0" | "false" | "False" | "FALSE"))
        .unwrap_or(true);

    // The SP1 guest programs live in their own nested cargo workspace at
    // `proofs/succinct/programs/`, but they have path dependencies that
    // reach outside that nested workspace (e.g. `world-chain-proof-core`
    // at `proofs/core`). By default `sp1_build` mounts the program's
    // cargo-metadata workspace root into the Docker container at
    // `/root/program`, which would only expose `proofs/succinct/programs/`
    // and break those out-of-workspace path deps (causing the container
    // to fail looking for `/core/Cargo.toml`).
    //
    // Mirror the op-succinct approach: explicitly set `workspace_directory`
    // to the top-level repo workspace root so the entire repository is
    // mounted into the Docker container. All path deps then resolve
    // identically to a local build.
    //
    // `CARGO_MANIFEST_DIR` for this build script is
    // `<repo>/proofs/succinct/utils/host`, so the repo root is four
    // ancestors up.
    let manifest_dir = std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR must be set by cargo for build scripts"),
    );
    let workspace_root = manifest_dir
        .ancestors()
        .nth(4)
        .expect("build.rs is expected to live at <repo>/proofs/succinct/utils/host")
        .to_path_buf();
    let workspace_root = workspace_root
        .to_str()
        .expect("workspace root path must be valid UTF-8")
        .to_string();

    let build = |program_dir: &str| {
        sp1_build::build_program_with_args(
            program_dir,
            sp1_build::BuildArgs {
                docker,
                tag: "v6.1.0".to_string(),
                ignore_rust_version: true,
                workspace_directory: Some(workspace_root.clone()),
                ..Default::default()
            },
        );
    };

    // Path is relative to this build script's CARGO_MANIFEST_DIR
    // (proofs/succinct/utils/host).
    build("../../programs/range-ethereum");
    build("../../programs/aggregation");
}
