#![cfg(target_os = "linux")]

use crate::cmd::run::CommonArgs;
use world_chain_proof_nitro::{
    ExpectedPcrs,
    host::{EnclaveEndpoint, NitroProver},
};

#[derive(clap::Args)]
pub struct GetAttestationArgs {
    #[command(flatten)]
    pub common: CommonArgs,
}

pub async fn get_attestation(args: GetAttestationArgs) -> anyhow::Result<()> {
    let prover = NitroProver::new(
        EnclaveEndpoint::with_port(args.common.enclave_cid, args.common.enclave_port),
        ExpectedPcrs::PLACEHOLDER,
    );

    let attestation_doc = prover
        .get_attestation()
        .await
        .map_err(|e| anyhow::anyhow!("get_attestation failed: {e}"))?;

    println!("{}", hex::encode(attestation_doc));
    Ok(())
}
