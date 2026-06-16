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
//! - Only runs when the `embedded-elfs` feature is enabled
//!   (`CARGO_FEATURE_EMBEDDED_ELFS` is set by Cargo). Builds with the `sp1`
//!   feature but without `embedded-elfs` skip the SP1 compile entirely and
//!   load ELFs at runtime from `RANGE_ELF_PATH` / `AGG_ELF_PATH` env vars
//!   (see `src/env_prover.rs`). This lets CI clippy/lint/test runs proceed
//!   without Docker or the SP1 toolchain.
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
//! - When invoked by `cargo clippy` (detected via `RUSTC_WORKSPACE_WRAPPER`),
//!   stub ELF files are written to `$OUT_DIR` and the `SP1_ELF_*` env vars
//!   point to them. This lets `include_elf!()` compile without Docker or a
//!   pre-built ELF cache. The stubs are zero-length and must not be used at
//!   runtime.

/// Names of the two SP1 guest programs whose ELF paths must be emitted.
const PROGRAMS: &[&str] = &[
    "world-chain-proof-succinct-range-ethereum",
    "world-chain-proof-succinct-aggregation",
];

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_EMBEDDED_ELFS");
    println!("cargo:rerun-if-env-changed=SP1_SKIP_PROGRAM_BUILD");
    println!("cargo:rerun-if-env-changed=SP1_BUILD_DOCKER");
    println!("cargo:rerun-if-env-changed=RUSTC_WORKSPACE_WRAPPER");

    // Without `embedded-elfs`, ELFs are loaded at runtime from env vars — no
    // compile-time embedding, no Docker build needed. CI builds take this path.
    if std::env::var_os("CARGO_FEATURE_EMBEDDED_ELFS").is_none() {
        return;
    }

    // When cargo clippy drives the build (RUSTC_WORKSPACE_WRAPPER points to
    // clippy-driver), skip the actual SP1/Docker compilation entirely.
    // sp1_build itself skips the Docker step in this case but still emits env
    // vars pointing to ELF paths that don't exist on a fresh checkout, causing
    // `include_elf!()` → `include_bytes!(env!("SP1_ELF_..."))` to fail at
    // compile time.  We intercept early, write zero-byte stub files to
    // OUT_DIR (which always exists), and emit vars pointing to those stubs so
    // that the macro compiles cleanly without Docker or a cached ELF.
    //
    // `cargo clippy --all-features` (as used by the rust-ci workflow) is the
    // primary trigger for this path.
    let is_clippy = std::env::var("RUSTC_WORKSPACE_WRAPPER")
        .map(|v| v.contains("clippy-driver"))
        .unwrap_or(false);
    if is_clippy {
        emit_stub_elfs();
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

/// Write zero-byte stub ELF files to `OUT_DIR` and emit `SP1_ELF_*` env vars
/// pointing to them.  Used during `cargo clippy` runs so that
/// `include_elf!()` compiles without a real ELF being present.
fn emit_stub_elfs() {
    let out_dir =
        std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is always set by Cargo"));

    for name in PROGRAMS {
        let stub = out_dir.join(format!("{name}.stub.elf"));
        std::fs::write(&stub, b"").unwrap_or_else(|e| {
            panic!(
                "failed to write stub ELF for {name} at {}: {e}",
                stub.display()
            )
        });
        println!("cargo:rustc-env=SP1_ELF_{name}={}", stub.display());
    }

    println!(
        "cargo:warning=embedded-elfs: clippy run detected — stub ELFs emitted. \
         These are not valid SP1 programs and must not be used at runtime."
    );
}
