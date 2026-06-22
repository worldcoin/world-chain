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
//! The release-tracked vkey (governance-tracked SHA-256 of each ELF) MUST come
//! from a bit-for-bit reproducible build. Three modes are supported, selected
//! by environment, in priority order:
//!
//! - **Embed prebuilt ELFs** via `SP1_PREBUILT_ELF_DIR=<dir>` (highest
//!   priority). When set, this script points `include_elf!()` at
//!   `<dir>/<program>` and does NOT compile anything. This is the *release*
//!   path: the ELFs are produced once by the reproducible `build-elfs` CI job
//!   (`docker: true`) and injected so every release image/binary embeds the
//!   same canonical, vkey-matching bytes. The files must exist at link time;
//!   `include_bytes!` fails loudly if one is missing (no silent fallback).
//!
//! - **Local (non-Docker) in-image build** via `SP1_BUILD_DOCKER=false`. Used
//!   by the non-release prover image builds (`Dockerfile.prover`): the guest is
//!   compiled by the in-image SP1 toolchain at the fixed `/app` WORKDIR with
//!   the pinned tag, so it is consistent across the image-build matrix without
//!   needing a Docker daemon or the artifact plumbing. These nightly/dev images
//!   are NOT vkey-canonical — releases use the prebuilt path above.
//!
//! - **SP1 Docker reproducibility builder** (`docker: true`, default). The
//!   container fixes the toolchain and path layout, so the ELF is identical
//!   regardless of host. Used wherever a Docker daemon is reachable and no
//!   prebuilt ELFs were supplied (dev machines, the `vkeys` / `build-elfs` CI
//!   jobs).
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
    println!("cargo:rerun-if-env-changed=SP1_BUILD_DOCKER");

    // Fast path for environments without a Docker daemon: embed prebuilt,
    // reproducibly-built ELFs instead of compiling. `include_elf!(name)` reads
    // `SP1_ELF_<name>`, so emitting an absolute path here makes it embed those
    // exact bytes with no guest compile. The files must exist at link time;
    // `include_bytes!` fails loudly if one is missing (no silent fallback).
    // Treat an empty value as unset so callers can opt out with `KEY=`.
    if let Ok(dir) = std::env::var("SP1_PREBUILT_ELF_DIR") {
        if !dir.trim().is_empty() {
            for program in PROGRAMS {
                println!("cargo:rustc-env=SP1_ELF_{program}={dir}/{program}");
            }
            return;
        }
    }

    // No prebuilt ELFs supplied: compile the guests. Use the SP1 Docker
    // reproducibility builder by default; `SP1_BUILD_DOCKER=false` switches to a
    // local in-image build (sp1-build does not read this var itself, so we honor
    // it here). The non-release prover image build sets it to compile the guests
    // with the in-image toolchain.
    let docker = !matches!(
        std::env::var("SP1_BUILD_DOCKER").as_deref(),
        Ok("false") | Ok("0")
    );

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
                docker,
                tag: "v6.1.0".to_string(),
                ignore_rust_version: true,
                // Only consumed by the Docker builder (mounts the repo root so
                // out-of-workspace path deps resolve); ignored by a local build.
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
