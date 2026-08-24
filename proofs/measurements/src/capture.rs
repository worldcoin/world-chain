//! Rebuilds every measurement from source and reconciles it with `measurements.toml`.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use eyre::eyre::{Context, bail, eyre};
use serde::Deserialize;

use crate::document::{Measurements, Nitro, PATH, SCHEMA_VERSION, Sp1, derive_tee_image_id};

/// nitro-cli tag whose blobs supply the kernel and bootstrap ramdisk, and so fix PCR1.
const NITRO_CLI_VERSION: &str = "v1.4.2";
/// Docker tag the Nix image is loaded under for `nitro-cli build-enclave`.
const IMAGE_TAG: &str = "world-chain-proof-nitro-enclave:nix";

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

/// Rebuilds everything; writes `measurements.toml`, or compares against it when `check`.
pub fn run(root: &Path, check: bool) -> eyre::Result<()> {
    let out = root.join("target/measurements");
    std::fs::create_dir_all(&out).wrap_err_with(|| format!("creating {}", out.display()))?;
    let path = root.join(PATH);

    let built = Measurements {
        version: SCHEMA_VERSION,
        sp1: measure_sp1(root, &out)?,
        nitro: measure_nitro(root, &out)?,
    };
    built.validate()?;

    if !check {
        std::fs::write(&path, built.render()?)
            .wrap_err_with(|| format!("writing {}", path.display()))?;
        println!("wrote {}", path.display());
        return Ok(());
    }

    let text = std::fs::read_to_string(&path).wrap_err_with(|| {
        format!(
            "cannot read {}; run `just measure` to generate it",
            path.display()
        )
    })?;
    let diff = Measurements::parse(&text)?.diff(&built);
    if diff.is_empty() {
        println!("{PATH} is up to date");
        return Ok(());
    }
    bail!(
        "{PATH} is stale — rebuilt measurements differ:\n{}\n\n\
         Run `just measure` (Linux x86_64 + Docker + Nix) and commit the result.",
        diff.join("\n")
    );
}

/// Rebuilds the SP1 guest ELFs and derives their vkeys.
///
/// Delegates to the existing `vkeys` subcommand rather than reimplementing the setup pass, so
/// the values recorded are produced by the code the prover actually runs.
fn measure_sp1(root: &Path, out: &Path) -> eyre::Result<Sp1> {
    let manifest = out.join("vkeys.json");
    run_cmd(
        Command::new("cargo")
            .current_dir(root)
            .env("SP1_BUILD_DOCKER", "true")
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
        "building the SP1 guest vkeys",
    )?;

    let text = std::fs::read_to_string(&manifest)
        .wrap_err_with(|| format!("reading {}", manifest.display()))?;
    let m: VkeyManifest =
        serde_json::from_str(&text).wrap_err_with(|| format!("parsing {}", manifest.display()))?;

    Ok(Sp1 {
        aggregation_vkey: m.aggregation_vkey,
        range_vkey_commitment: m.range_vkey_commitment,
        aggregation_elf_sha256: m.elfs.aggregation.sha256,
        range_elf_sha256: m.elfs.range.sha256,
    })
}

/// Builds the enclave image with Nix, converts it to an EIF, and records the PCRs.
fn measure_nitro(root: &Path, out: &Path) -> eyre::Result<Nitro> {
    require_linux_x86_64()?;

    // Nix gives a content hash of the whole rootfs closure for free. Recording it means a PCR
    // that moves while the store path did not points at the EIF assembly, not the image.
    let image_store_path = capture(
        Command::new("nix").current_dir(root).args([
            "build",
            ".#enclave-image",
            "--no-link",
            "--print-out-paths",
        ]),
        "building the enclave image with nix",
    )?
    .trim()
    .to_string();

    run_cmd(
        Command::new("docker")
            .arg("load")
            .arg("-i")
            .arg(&image_store_path),
        "loading the enclave image",
    )?;

    let (pcrs, cli_version) = build_eif(root, out)?;
    let tee_image_id = derive_tee_image_id(&pcrs.pcr0)?;

    Ok(Nitro {
        pcr0: pcrs.pcr0,
        pcr1: pcrs.pcr1,
        pcr2: pcrs.pcr2,
        tee_image_id,
        image_store_path,
        cli_version,
    })
}

/// Builds nitro-cli from its pinned tag and converts the loaded image to an EIF.
fn build_eif(root: &Path, out: &Path) -> eyre::Result<(Pcrs, String)> {
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
        // `--locked` so a floating transitive dependency cannot change the binary whose blobs
        // fix PCR1.
        run_cmd(
            Command::new("cargo")
                .args([
                    "build",
                    "--release",
                    "--locked",
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
    Ok((parsed.measurements, NITRO_CLI_VERSION.to_string()))
}

fn require_linux_x86_64() -> eyre::Result<()> {
    if !cfg!(target_os = "linux") || !cfg!(target_arch = "x86_64") {
        bail!(
            "measuring the enclave requires Linux x86_64 with Docker and Nix (EIFs are \
             linux/amd64 only); run this in CI or on a Linux builder"
        );
    }
    Ok(())
}

pub fn repo_root() -> eyre::Result<PathBuf> {
    Ok(PathBuf::from(
        capture(
            Command::new("git").args(["rev-parse", "--show-toplevel"]),
            "locating the repository root",
        )?
        .trim(),
    ))
}

/// Runs a command, surfacing its stderr on failure rather than a bare exit code.
pub fn run_cmd(cmd: &mut Command, what: &str) -> eyre::Result<()> {
    let status = cmd
        .status()
        .wrap_err_with(|| format!("{what}: failed to spawn {:?}", cmd.get_program()))?;
    if !status.success() {
        bail!("{what}: {:?} exited with {status}", cmd.get_program());
    }
    Ok(())
}

/// Runs a command and returns its stdout.
pub fn capture(cmd: &mut Command, what: &str) -> eyre::Result<String> {
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
