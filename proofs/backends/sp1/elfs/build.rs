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
//! - Uses `docker: true` by default with the pinned SP1 toolchain image
//!   (matches the `=6.3.1` version of `sp1-zkvm` the guest workspace pins to)
//!   for bit-for-bit reproducible ELFs. This is the ecosystem
//!   standard used by op-succinct, sp1-helios, and all other SP1 adopters.
//!   Docker provides reproducibility by fixing the build environment path
//!   layout inside the container.
//! - Set `SP1_BUILD_DOCKER=false` to use a locally-installed `cargo-prove`
//!   instead of the Docker image. This is required in `Dockerfile.prover`
//!   where Docker-in-Docker is unavailable, and optional for local development
//!   when a compatible `sp1up` toolchain is already installed.
//! - Honours `SP1_SKIP_PROGRAM_BUILD=true` for fast iteration: `sp1_build`
//!   checks this variable internally — when set, it skips the Docker/local
//!   guest compilation but **still emits** the `SP1_ELF_*` cargo env-vars so
//!   `include_elf!()` resolves against previously-cached ELFs in
//!   `target/elf-compilation/...`. Our `main` does not need a separate
//!   early-return for this flag; the delegation to `sp1_build` is sufficient.
//!   Useful for `cargo check` once a single full build has populated the
//!   target directory.
//! - Under `cargo clippy`, build scripts receive `CARGO_CFG_CLIPPY=1`.
//!   This script exits early in that case because the `#[cfg(clippy)]` guards
//!   in `src/lib.rs` prevent `include_elf!()` from expanding, so no ELF
//!   files need to exist and no SP1 build is required.

fn main() {
    println!("cargo:rerun-if-env-changed=SP1_SKIP_PROGRAM_BUILD");
    println!("cargo:rerun-if-env-changed=SP1_BUILD_DOCKER");

    // Under `cargo clippy` the build script receives `CARGO_CFG_CLIPPY=1`.
    // The `#[cfg(clippy)]` guards in `src/lib.rs` prevent `include_elf!()`
    // from expanding in that mode, so no ELF files need to exist and we can
    // skip the SP1 guest build entirely.
    if std::env::var("CARGO_CFG_CLIPPY").is_ok() {
        return;
    }

    // Respect `SP1_BUILD_DOCKER` to allow callers to opt out of Docker-based
    // compilation. Defaults to `true` for reproducible builds; set to `false`
    // when Docker-in-Docker is unavailable (e.g. inside `Dockerfile.prover`)
    // or when a local `cargo-prove` installation is preferred.
    let docker = std::env::var("SP1_BUILD_DOCKER")
        .map(|v| v.to_lowercase() != "false")
        .unwrap_or(true);

    // The SP1 guest programs live in their own nested cargo workspace at
    // `proofs/measured/sp1-programs/`, but they have path dependencies that
    // reach outside that nested workspace (e.g. `world-chain-proof-core`
    // at `proofs/measured/core`). By default `sp1_build` mounts the program's
    // cargo-metadata workspace root into the Docker container at
    // `/root/program`, which would only expose the programs workspace
    // and break those out-of-workspace path deps (causing the container
    // to fail looking for `/core/Cargo.toml`).
    //
    // Mirror the op-succinct approach: explicitly set `workspace_directory`
    // to the top-level repo workspace root so the entire repository is
    // mounted into the Docker container. All path deps then resolve
    // identically to a local build.
    //
    // `CARGO_MANIFEST_DIR` for this build script is
    // `<repo>/proofs/backends/sp1/elfs`, so the repo root is four levels up
    // (ancestors().nth(4) where nth(0) = self, nth(1) = backends/sp1,
    // nth(2) = backends, nth(3) = proofs, nth(4) = repo root).
    let manifest_dir = std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR must be set by cargo for build scripts"),
    );
    let workspace_root = manifest_dir
        .ancestors()
        .nth(4)
        .expect("build.rs is expected to live at <repo>/proofs/backends/sp1/elfs")
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
                docker,
                // Pin the linux/amd64 manifest that produced measurements.json. The tag remains in the
                // reference for readability, while the digest prevents a mutable tag from
                // silently rotating the guest ELFs and their on-chain vkeys.
                tag:
                    "v6.3.1@sha256:7c1c8201de6f63e3f1fb9075bd9a67a4c5fc8c2d546d11a5ff71587bb51e6eb3"
                        .to_string(),
                ignore_rust_version: true,
                locked: true,
                workspace_directory: Some(workspace_root.clone()),
                ..Default::default()
            },
        );
    };

    // Paths are relative to this build script's CARGO_MANIFEST_DIR
    // (proofs/backends/sp1/elfs).
    build("../programs/range-ethereum");
    build("../programs/aggregation");
}
