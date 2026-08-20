//! Rebuilds every proof measurement from source and reconciles it with `measurements.lock`.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use eyre::eyre::{Context, bail, eyre};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::measure::lock::{
    LOCK_PATH, Lock, Nitro, SCHEMA_VERSION, Sp1, Sp1Elf, derive_tee_image_id,
};

/// nitro-cli tag whose blobs supply the kernel and bootstrap ramdisk, and so fix PCR1.
const NITRO_CLI_VERSION: &str = "v1.4.2";
/// Fixed timestamp BuildKit rewrites every layer to; changing it rotates the PCRs.
const SOURCE_DATE_EPOCH: &str = "1780272000";
/// Docker tag for the intermediate enclave image.
const IMAGE_TAG: &str = "world-chain-proof-nitro-enclave:measure";
/// Where the enclave binary lands inside the image.
const ENCLAVE_BINARY: &str = "/usr/local/bin/world-chain-proof-nitro-enclave";
/// Enclave Dockerfile, relative to the repo root.
const DOCKERFILE: &str = "proofs/backends/nitro/enclave/Dockerfile";

/// `cargo xtask measure`
#[derive(Debug, clap::Args)]
pub struct Args {
    /// Rebuild and compare against the committed lock instead of writing it. Used by CI.
    #[arg(long)]
    pub check: bool,
}

/// What `world-chain-prover-sp1 vkeys --output` writes.
#[derive(Debug, Deserialize)]
struct VkeyManifest {
    aggregation_vkey: String,
    range_vkey_commitment: String,
    elfs: VkeyElfs,
}

#[derive(Debug, Deserialize)]
struct VkeyElfs {
    #[serde(rename = "world-chain-range-ethereum")]
    range: VkeyElf,
    #[serde(rename = "world-chain-aggregation")]
    aggregation: VkeyElf,
}

#[derive(Debug, Deserialize)]
struct VkeyElf {
    sha256: String,
}

/// The `Measurements` object `nitro-cli build-enclave` reports.
#[derive(Debug, Deserialize)]
struct Pcrs {
    #[serde(rename = "PCR0")]
    pcr0: String,
    #[serde(rename = "PCR1")]
    pcr1: String,
    #[serde(rename = "PCR2")]
    pcr2: String,
}

pub fn run(args: Args) -> eyre::Result<()> {
    let root = repo_root()?;
    let out = root.join("target/measure");
    std::fs::create_dir_all(&out).wrap_err_with(|| format!("creating {}", out.display()))?;

    let path = root.join(LOCK_PATH);
    // Read the committed lock first when checking, so its digest can be handed to the image
    // build and enforced there too, not just compared afterwards.
    let committed = if args.check {
        let text = std::fs::read_to_string(&path).wrap_err_with(|| {
            format!(
                "cannot read {}; run `cargo xtask measure` to generate it",
                path.display()
            )
        })?;
        Some(Lock::parse(&text)?)
    } else {
        None
    };

    let built = Lock {
        version: SCHEMA_VERSION,
        sp1: measure_sp1(&root, &out)?,
        nitro: measure_nitro(
            &root,
            &out,
            committed.as_ref().map(|l| l.nitro.binary_sha256.as_str()),
        )?,
    };
    built.validate()?;

    if !args.check {
        std::fs::write(&path, built.render()?)
            .wrap_err_with(|| format!("writing {}", path.display()))?;
        println!("wrote {}", path.display());
        return Ok(());
    }

    let committed = committed.expect("read above when --check is set");
    let diff = committed.diff(&built);
    if diff.is_empty() {
        println!("{LOCK_PATH} is up to date");
        return Ok(());
    }
    bail!(
        "{LOCK_PATH} is stale — rebuilt measurements differ:\n{}\n\n\
         Run `cargo xtask measure` (Linux x86_64 + Docker) and commit the result.",
        diff.join("\n")
    );
}

/// Rebuilds the SP1 guest ELFs and derives their vkeys.
///
/// Delegates to the existing `vkeys` subcommand rather than reimplementing the setup pass,
/// so the values here are produced by exactly the code the prover uses at runtime.
fn measure_sp1(root: &Path, out: &Path) -> eyre::Result<Sp1> {
    let manifest = out.join("vkeys.json");
    run_cmd(
        Command::new("cargo")
            .current_dir(root)
            .env("SP1_BUILD_DOCKER", "true")
            // We are producing the lock, so the guest build must not assert against it.
            .env("WORLD_CHAIN_MEASURE_BOOTSTRAP", "1")
            .args([
                "run",
                "--release",
                "-p",
                "world-chain-prover-sp1",
                "--",
                "vkeys",
                "--output",
            ])
            .arg(&manifest),
        "building SP1 guest vkeys",
    )?;

    let text = std::fs::read_to_string(&manifest)
        .wrap_err_with(|| format!("reading {}", manifest.display()))?;
    let m: VkeyManifest =
        serde_json::from_str(&text).wrap_err_with(|| format!("parsing {}", manifest.display()))?;

    Ok(Sp1 {
        aggregation_vkey: m.aggregation_vkey,
        range_vkey_commitment: m.range_vkey_commitment,
        elf: Sp1Elf {
            aggregation_sha256: m.elfs.aggregation.sha256,
            range_sha256: m.elfs.range.sha256,
        },
    })
}

/// Builds the enclave image and its EIF, and records both the PCRs and the cheap input
/// digests a build can assert without producing an EIF.
fn measure_nitro(root: &Path, out: &Path, expected_binary: Option<&str>) -> eyre::Result<Nitro> {
    require_linux_x86_64()?;

    build_image(root, out, expected_binary)?;
    let binary_sha256 = image_binary_sha256(out)?;
    let dockerfile_sha256 = sha256_file(&root.join(DOCKERFILE))?;
    let pcrs = build_eif(root, out)?;
    let tee_image_id = derive_tee_image_id(&pcrs.pcr0)?;

    Ok(Nitro {
        pcr0: pcrs.pcr0,
        pcr1: pcrs.pcr1,
        pcr2: pcrs.pcr2,
        tee_image_id,
        binary_sha256,
        dockerfile_sha256,
        cli_version: NITRO_CLI_VERSION.to_string(),
    })
}

/// Builds the enclave image reproducibly.
///
/// `rewrite-timestamp` normalises every layer's timestamps, which is what keeps PCR0/PCR2
/// stable across builds; it cannot be combined with an exporter that unpacks straight into
/// the image store, hence the archive-then-load two-step.
fn build_image(root: &Path, out: &Path, expected_binary: Option<&str>) -> eyre::Result<()> {
    let archive = out.join("enclave-image.tar");
    // When checking, hand the recorded digest to the build so the image build itself rejects
    // a binary that disagrees. When generating, say so explicitly — there is nothing to
    // check against yet.
    let (bootstrap, expected) = match expected_binary {
        Some(digest) => ("0", digest),
        None => ("1", ""),
    };
    run_cmd(
        Command::new("docker")
            .current_dir(root)
            .env("SOURCE_DATE_EPOCH", SOURCE_DATE_EPOCH)
            // EIFs are linux/amd64 only, so pin the platform rather than inheriting the
            // host's: an arm64 build would silently produce a different binary and PCRs.
            .args([
                "buildx",
                "build",
                "--platform",
                "linux/amd64",
                "--build-arg",
            ])
            .arg(format!("MEASURE_BOOTSTRAP={bootstrap}"))
            .arg("--build-arg")
            .arg(format!("EXPECTED_BINARY_SHA256={expected}"))
            .arg("--build-arg")
            .arg(format!("SOURCE_DATE_EPOCH={SOURCE_DATE_EPOCH}"))
            .arg("--output")
            .arg(format!(
                "type=docker,name={IMAGE_TAG},dest={},rewrite-timestamp=true",
                archive.display()
            ))
            .args(["-f", DOCKERFILE, "."]),
        "building the enclave container image",
    )?;
    run_cmd(
        Command::new("docker").arg("load").arg("-i").arg(&archive),
        "loading the enclave image",
    )?;
    std::fs::remove_file(&archive).ok();
    Ok(())
}

/// SHA-256 of the enclave binary as it exists inside the image.
///
/// Copied out of a created (never started) container so the hash covers the exact bytes the
/// EIF will carry, without needing to execute a linux/amd64 binary.
fn image_binary_sha256(out: &Path) -> eyre::Result<String> {
    let id = capture(
        Command::new("docker").args(["create", IMAGE_TAG]),
        "creating a container to read the enclave binary",
    )?;
    let id = id.trim();
    let local = out.join("world-chain-proof-nitro-enclave");
    let copy = run_cmd(
        Command::new("docker")
            .arg("cp")
            .arg(format!("{id}:{ENCLAVE_BINARY}"))
            .arg(&local),
        "copying the enclave binary out of the image",
    );
    // Remove the container regardless, so a failure here does not leak one.
    let _ = Command::new("docker").args(["rm", id]).output();
    copy?;

    let digest = sha256_file(&local)?;
    std::fs::remove_file(&local).ok();
    Ok(digest)
}

/// Builds nitro-cli from a pinned tag, converts the image to an EIF, and returns its PCRs.
fn build_eif(root: &Path, out: &Path) -> eyre::Result<Pcrs> {
    let cli_dir = out.join(format!("aws-nitro-enclaves-cli-{NITRO_CLI_VERSION}"));
    let cli = cli_dir.join("target/release/nitro-cli");
    if !cli.is_file() {
        std::fs::remove_dir_all(&cli_dir).ok();
        run_cmd(
            Command::new("git")
                .args([
                    "clone",
                    "--depth",
                    "1",
                    "--branch",
                    NITRO_CLI_VERSION,
                    "https://github.com/aws/aws-nitro-enclaves-cli",
                ])
                .arg(&cli_dir),
            "cloning aws-nitro-enclaves-cli",
        )?;
        run_cmd(
            Command::new("cargo")
                .args([
                    "build",
                    "--release",
                    "--bin",
                    "nitro-cli",
                    "--manifest-path",
                ])
                .arg(cli_dir.join("Cargo.toml")),
            "building nitro-cli",
        )?;
    }

    let eif = out.join("world-chain-proof-nitro-enclave.eif");
    let json = capture(
        Command::new(&cli)
            .current_dir(root)
            .env("NITRO_CLI_BLOBS", cli_dir.join("blobs/x86_64"))
            .env("NITRO_CLI_ARTIFACTS", out.join("artifacts"))
            .args(["build-enclave", "--docker-uri", IMAGE_TAG, "--output-file"])
            .arg(&eif),
        "converting the enclave image to an EIF",
    )?;

    #[derive(Deserialize)]
    struct BuildOutput {
        #[serde(rename = "Measurements")]
        measurements: Pcrs,
    }
    // nitro-cli prints progress before the JSON object, so start from the first brace.
    let start = json
        .find('{')
        .ok_or_else(|| eyre!("nitro-cli build-enclave produced no JSON:\n{json}"))?;
    let parsed: BuildOutput =
        serde_json::from_str(&json[start..]).wrap_err("parsing nitro-cli build-enclave output")?;
    Ok(parsed.measurements)
}

fn require_linux_x86_64() -> eyre::Result<()> {
    if !cfg!(target_os = "linux") || !cfg!(target_arch = "x86_64") {
        bail!(
            "measuring the enclave requires Linux x86_64 with Docker (EIF builds are \
             linux/amd64 only); run this in CI or on a Linux builder"
        );
    }
    Ok(())
}

fn repo_root() -> eyre::Result<PathBuf> {
    let out = capture(
        Command::new("git").args(["rev-parse", "--show-toplevel"]),
        "locating the repository root",
    )?;
    Ok(PathBuf::from(out.trim()))
}

fn sha256_file(path: &Path) -> eyre::Result<String> {
    let bytes = std::fs::read(path).wrap_err_with(|| format!("reading {}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

/// Runs a command, surfacing its stderr on failure rather than a bare exit code.
fn run_cmd(cmd: &mut Command, what: &str) -> eyre::Result<()> {
    let status = cmd
        .status()
        .wrap_err_with(|| format!("{what}: failed to spawn {:?}", cmd.get_program()))?;
    if !status.success() {
        bail!("{what}: {:?} exited with {status}", cmd.get_program());
    }
    Ok(())
}

/// Runs a command and returns its stdout.
fn capture(cmd: &mut Command, what: &str) -> eyre::Result<String> {
    let output = cmd
        .output()
        .wrap_err_with(|| format!("{what}: failed to spawn {:?}", cmd.get_program()))?;
    if !output.status.success() {
        bail!(
            "{what}: {:?} exited with {}\n{}",
            cmd.get_program(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).wrap_err_with(|| format!("{what}: stdout is not UTF-8"))
}
