//! Build script: make the World Chain SP1 guest program ELFs available to
//! `sp1_sdk::include_elf!()` by emitting the `SP1_ELF_<crate>` environment
//! variables it reads (`include_elf!(name)` expands to
//! `include_bytes!(env!("SP1_ELF_<name>"))`).
//!
//! This is the OP Succinct upstream pattern (see `utils/build/` in
//! `succinctlabs/op-succinct`): the ELF bytes are embedded into the host
//! binary at link time via `include_elf!()` — there is no runtime
//! `fs::read` of an ELF file.
//!
//! ELFs MUST be bit-for-bit reproducible, because their SHA-256 and the
//! derived on-chain verification keys are governance-tracked and must be
//! identical across every build (including each arch of a multi-arch image).
//! Two mutually exclusive modes provide that:
//!
//! - **Build via the SP1 Docker reproducibility builder** (`docker: true`,
//!   pinned tag `v6.1.0` matching the `=6.1.0` `sp1-sdk` pin). The container
//!   fixes the toolchain and path layout, so the ELF is identical regardless
//!   of host. This is the default and is used wherever a Docker daemon is
//!   reachable (CI runners, dev machines, the `build-elfs` job). A *local*
//!   `cargo-prove` build is deliberately NOT offered as a fallback: local
//!   builds are not reproducible (host arch / paths leak into the ELF), which
//!   would diverge the vkey.
//!
//! - **Embed prebuilt ELFs** via `SP1_PREBUILT_ELF_DIR=<dir>`. When set, this
//!   script points `include_elf!()` at `<dir>/<program>` and does NOT compile
//!   anything. This is for environments without a Docker daemon — notably the
//!   prover image build (`Dockerfile.prover`, no Docker-in-Docker) — where the
//!   ELFs produced once by the `build-elfs` CI job (`docker: true`) are
//!   downloaded and injected, so every image embeds the same canonical bytes.
//!
//! Under `cargo clippy`, the `#[cfg(clippy)]` guards in `src/lib.rs` prevent
//! `include_elf!()` from expanding, so no ELF files need to exist.

/// Guest program bin-target names, matching the `include_elf!(...)` arguments
/// in `src/lib.rs` and the file names produced/consumed by the build-elfs job.
const PROGRAMS: [&str; 2] = [
    "world-chain-proof-succinct-range-ethereum",
    "world-chain-proof-succinct-aggregation",
];

fn main() {
    println!("cargo:rerun-if-env-changed=SP1_PREBUILT_ELF_DIR");

    // Fast path for environments without a Docker daemon: embed prebuilt,
    // reproducibly-built ELFs instead of compiling. `include_elf!(name)` reads
    // `SP1_ELF_<name>`, so emitting an absolute path here makes it embed those
    // exact bytes with no guest compile. The files must exist at link time;
    // `include_bytes!` fails loudly if one is missing (no silent fallback).
    if let Ok(dir) = std::env::var("SP1_PREBUILT_ELF_DIR") {
        for program in PROGRAMS {
            println!("cargo:rustc-env=SP1_ELF_{program}={dir}/{program}");
        }
        return;
    }

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
                docker: true,
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
