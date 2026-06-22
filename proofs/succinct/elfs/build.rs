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
//! - Defaults to `docker: true` with the pinned SP1 toolchain tag
//!   (matches the `=6.1.0` version of `sp1-sdk` / `sp1-zkvm` the workspace
//!   pins to) for bit-for-bit reproducible ELFs. This is the ecosystem
//!   standard used by op-succinct, sp1-helios, and all other SP1 adopters.
//!   Docker provides reproducibility by fixing the build environment path
//!   layout inside the container.
//! - Honours `SP1_BUILD_DOCKER=false` to fall back to the locally-installed
//!   `cargo-prove` toolchain instead of the Docker reproducibility builder.
//!   This is required wherever the Docker daemon is unreachable from inside
//!   the build — notably `Dockerfile.prover` (no Docker-in-Docker) and the
//!   `vkeys` CI step, both of which install the pinned `v6.1.0` toolchain via
//!   `sp1up` and set this variable. `sp1_build` does not read this variable
//!   itself, so we map it onto `BuildArgs.docker` here.
//! - Honours `SP1_SKIP_PROGRAM_BUILD=true` for fast iteration: `sp1_build`
//!   checks this variable internally — when set, it skips the Docker/local
//!   guest compilation but **still emits** the `SP1_ELF_*` cargo env-vars so
//!   `include_elf!()` resolves against previously-cached ELFs in
//!   `target/elf-compilation/...`. Our `main` does not need a separate
//!   early-return for this flag; the delegation to `sp1_build` is sufficient.
//!   Useful for `cargo check` once a single full build has populated the
//!   target directory. (Under `cargo clippy` the `#[cfg(clippy)]` guards in
//!   `src/lib.rs` already prevent `include_elf!()` from expanding, so no
//!   build is needed at all.)
//! - Under `cargo clippy`, the `#[cfg(clippy)]` guards in `src/lib.rs`
//!   prevent `include_elf!()` from expanding, so no ELF files need to exist.

fn main() {
    println!("cargo:rerun-if-env-changed=SP1_SKIP_PROGRAM_BUILD");
    println!("cargo:rerun-if-env-changed=SP1_BUILD_DOCKER");

    // Use the Docker reproducibility builder by default; allow opting out via
    // `SP1_BUILD_DOCKER=false` for environments where the Docker daemon is
    // unreachable (Dockerfile.prover, vkeys CI). Any value other than an
    // explicit "false" keeps the default Docker build.
    let use_docker = std::env::var("SP1_BUILD_DOCKER")
        .map(|v| !v.eq_ignore_ascii_case("false"))
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
    // `<repo>/proofs/succinct/elfs`, so the repo root is three levels up
    // (ancestors().nth(3) where nth(0) = self, nth(1) = proofs/succinct,
    // nth(2) = proofs, nth(3) = repo root).
    let manifest_dir = std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR must be set by cargo for build scripts"),
    );
    let workspace_root = manifest_dir
        .ancestors()
        .nth(3)
        .expect("build.rs is expected to live at <repo>/proofs/succinct/elfs")
        .to_path_buf();
    // Canonicalize so that the path passed to Docker matches the actual
    // absolute path on the host (resolves any symlinks in the checkout path).
    let workspace_root = workspace_root
        .canonicalize()
        .unwrap_or(workspace_root)
        .to_str()
        .expect("workspace root path must be valid UTF-8")
        .to_string();

    let build = |program_dir: &str| {
        sp1_build::build_program_with_args(
            program_dir,
            sp1_build::BuildArgs {
                docker: use_docker,
                tag: "v6.1.0".to_string(),
                ignore_rust_version: true,
                workspace_directory: Some(workspace_root.clone()),
                ..Default::default()
            },
        );
    };

    // Paths are relative to this build script's CARGO_MANIFEST_DIR
    // (proofs/succinct/elfs).
    build("../programs/range-ethereum");
    build("../programs/aggregation");
}
