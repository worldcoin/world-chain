#![cfg(target_os = "linux")]

use anyhow::{Context, Result};
use world_chain_nitro_worker::build_expected_pcrs;
use world_chain_proof_nitro::register::{
    RegisterParams, RegistrationOutcome, register_enclave_key,
};

use crate::cmd::common::CommonArgs;

/// Register the enclave's generated signing key on-chain.
///
/// Fetches a public-key-embedding attestation from the running enclave over vsock, builds
/// the `registerKey(attestationTbs, signature, attestationSigHints)` calldata, and submits
/// it to `NitroEnclaveKeyRegistry` on L1. Idempotent: exits successfully if the key is
/// already registered.
///
/// Prerequisites: CertManager pre-warmed and the enclave PCR set approved on
/// `NitroAttestationVerifier` (see `just proof-setup`). `registerKey` is not owner-gated.
#[derive(Debug, clap::Args)]
pub struct RegisterArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// `NitroEnclaveKeyRegistry` contract address on L1.
    #[arg(long, env = "NITRO_ENCLAVE_KEY_REGISTRY")]
    pub registry: String,

    /// L1 execution RPC URL to submit `registerKey` to.
    #[arg(long, env = "L1_RPC_URL")]
    pub l1_rpc: String,

    /// Hex-encoded private key used to sign and pay for the `registerKey` transaction.
    /// Falls back to `PRIVATE_KEY` when unset.
    #[arg(long, env = "REGISTER_PRIVATE_KEY", hide_env_values = true)]
    pub private_key: Option<String>,

    /// PCR0 hex (48 bytes). When all three PCRs are set the attestation is verified
    /// host-side before submission; otherwise host-side checks are skipped (the on-chain
    /// verifier still enforces the approved PCR allowlist).
    #[arg(long, env = "PCR0")]
    pub pcr0: Option<String>,

    /// PCR1 hex (48 bytes).
    #[arg(long, env = "PCR1")]
    pub pcr1: Option<String>,

    /// PCR2 hex (48 bytes).
    #[arg(long, env = "PCR2")]
    pub pcr2: Option<String>,
}

pub async fn register(args: RegisterArgs) -> Result<()> {
    let expected_pcrs = build_expected_pcrs(
        args.pcr0.as_deref(),
        args.pcr1.as_deref(),
        args.pcr2.as_deref(),
    )?;

    let private_key = args
        .private_key
        .or_else(|| std::env::var("PRIVATE_KEY").ok())
        .context("no registration key: set --private-key, REGISTER_PRIVATE_KEY, or PRIVATE_KEY")?;

    let outcome = register_enclave_key(RegisterParams {
        enclave_cid: args.common.enclave_cid,
        enclave_port: args.common.enclave_port,
        expected_pcrs,
        l1_rpc_url: args.l1_rpc,
        registry: args.registry,
        private_key,
    })
    .await?;

    match outcome {
        RegistrationOutcome::AlreadyRegistered => {
            println!("enclave key already registered on-chain");
        }
        RegistrationOutcome::Registered { tx_hash } => {
            println!("enclave key registered on-chain (tx {tx_hash})");
        }
    }
    Ok(())
}
