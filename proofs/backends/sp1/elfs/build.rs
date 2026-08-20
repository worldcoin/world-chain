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
    // `proofs/backends/sp1/programs/`, but they have path dependencies that
    // reach outside that nested workspace (e.g. `world-chain-proof-core`
    // at `proofs/core`). By default `sp1_build` mounts the program's
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

    let build_args = || sp1_build::BuildArgs {
        docker,
        // Pin the linux/amd64 manifest that produced the registry's vkeys. The tag remains in the
        // reference for readability, while the digest prevents a mutable tag from
        // silently rotating the guest ELFs and their on-chain vkeys.
        tag: "v6.3.1@sha256:7c1c8201de6f63e3f1fb9075bd9a67a4c5fc8c2d546d11a5ff71587bb51e6eb3"
            .to_string(),
        ignore_rust_version: true,
        locked: true,
        workspace_directory: Some(workspace_root.clone()),
        ..Default::default()
    };

    let lock_path = std::path::Path::new(&workspace_root).join("measurements.lock");
    println!("cargo:rerun-if-changed={}", lock_path.display());
    println!("cargo:rerun-if-env-changed=WORLD_CHAIN_MEASURE_BOOTSTRAP");

    // `cargo xtask measure` is what writes measurements.lock, and it links this crate, so it
    // cannot also require the file to already exist and agree. This is the only way to build
    // without the check, it has to be asked for explicitly, and it is set by `measure`
    // itself — a missing lock in any other build is a hard error, not a silent skip.
    let expected = if std::env::var_os("WORLD_CHAIN_MEASURE_BOOTSTRAP").is_some() {
        println!(
            "cargo:warning=WORLD_CHAIN_MEASURE_BOOTSTRAP set: not checking guest ELFs against measurements.lock"
        );
        None
    } else {
        Some(ExpectedElfDigests::load(&lock_path))
    };

    // Paths are relative to this build script's CARGO_MANIFEST_DIR
    // (proofs/backends/sp1/elfs).
    for (program_dir, field) in [
        ("../programs/range-ethereum", "range_sha256"),
        ("../programs/aggregation", "aggregation_sha256"),
    ] {
        let args = build_args();
        sp1_build::build_program_with_args(program_dir, args);
        if let Some(expected) = &expected {
            assert_elf_matches_lock(program_dir, &build_args(), field, expected);
        }
    }
}

/// The `sp1.elf` digests recorded in `measurements.lock`.
struct ExpectedElfDigests {
    aggregation_sha256: String,
    range_sha256: String,
}

impl ExpectedElfDigests {
    fn load(path: &std::path::Path) -> Self {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!(
                "cannot read {}: {e}\n\
                 The guest ELFs are measured against it. Run `cargo xtask measure` to generate it.",
                path.display()
            )
        });
        let doc: toml::Value = toml::from_str(&text)
            .unwrap_or_else(|e| panic!("{} is not valid TOML: {e}", path.display()));
        let get = |field: &str| -> String {
            doc.get("sp1")
                .and_then(|sp1| sp1.get("elf"))
                .and_then(|elf| elf.get(field))
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| {
                    panic!("{}: missing sp1.elf.{field}", path.display());
                })
                .to_string()
        };
        Self {
            aggregation_sha256: get("aggregation_sha256"),
            range_sha256: get("range_sha256"),
        }
    }

    fn field(&self, name: &str) -> &str {
        match name {
            "aggregation_sha256" => &self.aggregation_sha256,
            "range_sha256" => &self.range_sha256,
            other => panic!("unknown ELF digest field {other}"),
        }
    }
}

/// Fails the build when a freshly compiled guest ELF does not match `measurements.lock`.
///
/// The vkeys are a deterministic function of the ELF, so matching the ELF digest is what
/// makes it impossible to produce a guest whose vkey disagrees with the recorded one —
/// without paying for an SP1 setup pass on every build.
fn assert_elf_matches_lock(
    program_dir: &str,
    args: &sp1_build::BuildArgs,
    field: &str,
    expected: &ExpectedElfDigests,
) {
    use sha2::{Digest, Sha256};

    let manifest = std::path::Path::new(program_dir).join("Cargo.toml");
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(&manifest)
        .exec()
        .unwrap_or_else(|e| panic!("cargo metadata for {}: {e}", manifest.display()));
    let elf_paths = sp1_build::generate_elf_paths(&metadata, Some(args))
        .unwrap_or_else(|e| panic!("resolving ELF paths for {program_dir}: {e}"));

    for (target, elf_path) in elf_paths {
        // `SP1_SKIP_PROGRAM_BUILD` leaves whatever the last real build produced; verify it
        // when it exists (a stale cache is exactly what this guards) and stay quiet when it
        // does not, so `cargo check` on a fresh clone still works.
        if !elf_path.as_std_path().exists() {
            continue;
        }
        let bytes = std::fs::read(elf_path.as_std_path())
            .unwrap_or_else(|e| panic!("reading {elf_path}: {e}"));
        let built = format!("{:x}", Sha256::digest(&bytes));
        let want = expected.field(field);
        if built != want {
            panic!(
                "guest ELF `{target}` does not match measurements.lock\n  \
                 sp1.elf.{field}\n    lock:  {want}\n    built: {built}\n\
                 Run `cargo xtask measure` and commit the updated measurements.lock."
            );
        }
    }
}
