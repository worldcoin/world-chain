#![cfg(target_os = "linux")]

use anyhow::Result;
use world_chain_proof_nitro_enclave::ExpectedPcrs;
use world_chain_proof_nitro_register::{
    RegisterParams, RegistrationOutcome, register_enclave_key,
};

use crate::cmd::{common::CommonArgs, select_registration_signer};

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
    /// Falls back to `PRIVATE_KEY` when neither registration signer is set.
    #[arg(long, env = "REGISTER_PRIVATE_KEY", hide_env_values = true)]
    pub private_key: Option<String>,

    /// AWS KMS key ID or alias used to sign and pay for the `registerKey` transaction.
    /// Mutually exclusive with `private_key`.
    #[arg(long, env = "REGISTER_KMS_KEY_ID", hide_env_values = true)]
    pub kms_key_id: Option<String>,
}

pub async fn register(args: RegisterArgs) -> Result<()> {
    let fallback_private_key = std::env::var("PRIVATE_KEY").ok();
    let (signer_secret, signer_type) = select_registration_signer(
        args.private_key.as_deref(),
        args.kms_key_id.as_deref(),
        fallback_private_key.as_deref(),
    )?;

    let outcome = register_enclave_key(RegisterParams {
        enclave_cid: args.common.enclave_cid,
        enclave_port: args.common.enclave_port,
        expected_pcrs: ExpectedPcrs::PLACEHOLDER,
        l1_rpc_url: args.l1_rpc,
        registry: args.registry,
        signer_secret: signer_secret.to_owned(),
        signer_type,
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
