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
//! - By default uses `docker: true` with the pinned SP1 toolchain tag
//!   (matches the `=6.1.0` version of `sp1-sdk` / `sp1-zkvm` the workspace
//!   pins to) for bit-for-bit reproducible ELFs. Set
//!   `SP1_BUILD_DOCKER=false` to use a locally-installed `cargo-prove` /
//!   `sp1up` toolchain instead — useful inside container builds where the
//!   Docker daemon isn't reachable.
//! - When building locally (without Docker), reproducibility flags are
//!   injected via `BuildArgs::rustflags` so the resulting ELF bytes are
//!   identical regardless of where the repository is checked out or which
//!   user runs the build:
//!     * `-C debuginfo=0`           – no DWARF sections (no embedded paths)
//!     * `--remap-path-prefix`      – normalize workspace, cargo-home, and
//!                                    rustup-home to canonical placeholders
//!   Docker builds don't need these flags because the container's fixed paths
//!   already guarantee reproducibility.
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
    // Canonicalize so that rustc's --remap-path-prefix matches the actual
    // absolute paths it sees (resolves any symlinks in the checkout path).
    let workspace_root = workspace_root
        .canonicalize()
        .unwrap_or(workspace_root)
        .to_str()
        .expect("workspace root path must be valid UTF-8")
        .to_string();

    // Reproducibility flags for local (non-Docker) builds.
    //
    // Without Docker the guest ELF is compiled on the host machine, which
    // means absolute file paths (embedded by rustc for panic messages and
    // any residual debug info) vary across machines and checkout locations.
    // We fix this with --remap-path-prefix so the compiler always sees the
    // same canonical placeholder strings, producing byte-for-bit identical
    // ELFs on every machine.
    //
    // BuildArgs::rustflags entries are appended to CARGO_ENCODED_RUSTFLAGS
    // by sp1-build as \x1f-separated tokens, so each flag *word* must be a
    // separate Vec element (e.g. ["--remap-path-prefix", "old=new"]).
    //
    // Docker builds don't need these: the container mounts the repo at a
    // fixed path (/root/program) so all embedded paths are already uniform.
    let reproducibility_flags: Vec<String> = if docker {
        vec![]
    } else {
        let cargo_home = std::env::var("CARGO_HOME").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
            format!("{home}/.cargo")
        });
        let rustup_home = std::env::var("RUSTUP_HOME").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
            format!("{home}/.rustup")
        });

        vec![
            // Strip any residual debug info so DWARF sections (which can
            // embed full source paths) are not present in the ELF.
            "-C".to_string(),
            "debuginfo=0".to_string(),
            // Remap workspace source paths (e.g. /home/alice/world-chain →
            // /build) so panic location strings are machine-independent.
            "--remap-path-prefix".to_string(),
            format!("{workspace_root}=/build"),
            // Remap cargo registry / git source paths.
            "--remap-path-prefix".to_string(),
            format!("{cargo_home}=/cargo"),
            // Remap rustup toolchain / stdlib sysroot paths.
            "--remap-path-prefix".to_string(),
            format!("{rustup_home}=/rustup"),
        ]
    };

    let build = |program_dir: &str| {
        sp1_build::build_program_with_args(
            program_dir,
            sp1_build::BuildArgs {
                docker,
                tag: "v6.1.0".to_string(),
                ignore_rust_version: true,
                workspace_directory: Some(workspace_root.clone()),
                rustflags: reproducibility_flags.clone(),
                ..Default::default()
            },
        );
    };

    // Paths are relative to this build script's CARGO_MANIFEST_DIR
    // (proofs/succinct/elfs).
    build("../programs/range-ethereum");
    build("../programs/aggregation");
}
