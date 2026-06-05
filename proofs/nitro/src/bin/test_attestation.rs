#[cfg(target_os = "linux")]
use anyhow::Context;
#[cfg(target_os = "linux")]
use world_chain_proof_core::{
    range::WorldRangeHardforkConfig,
    witness::{BlobData, WorldRangeWitnessData, preimage_store::PreimageStore},
};
#[cfg(target_os = "linux")]
use world_chain_proof_nitro::{
    ExpectedPcrs, NitroRangeProofRequest,
    attestation::parse_and_check_pcrs,
    host::{EnclaveEndpoint, NitroProver},
    protocol::range_user_data,
};

#[cfg(target_os = "linux")]
fn hex_to_pcr(hex: &str) -> anyhow::Result<[u8; 48]> {
    let bytes = hex::decode(hex).context("invalid hex")?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("PCR must be 48 bytes"))
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .init();

    let pcr0 = std::env::var("PCR0").context("PCR0 env var required")?;
    let pcr1 = std::env::var("PCR1").context("PCR1 env var required")?;
    let pcr2 = std::env::var("PCR2").context("PCR2 env var required")?;
    let cid: u32 = std::env::var("ENCLAVE_CID")
        .unwrap_or_else(|_| "16".into())
        .parse()
        .context("ENCLAVE_CID must be a u32")?;

    let expected_pcrs = ExpectedPcrs {
        pcr0: hex_to_pcr(&pcr0)?,
        pcr1: hex_to_pcr(&pcr1)?,
        pcr2: hex_to_pcr(&pcr2)?,
    };

    let witness = WorldRangeWitnessData::from_parts_with_world_config(
        PreimageStore::default(),
        BlobData::default(),
        WorldRangeHardforkConfig::default(),
    );
    let request = NitroRangeProofRequest::from_witness_data(&witness, None)?;

    let prover = NitroProver::new(EnclaveEndpoint::new(cid), expected_pcrs);

    tracing::info!(cid, "sending range proof request to enclave");
    let artifact = prover.prove_range_async(request).await?;

    tracing::info!(
        l2_pre_root  = ?artifact.boot_info.l2PreRoot,
        l2_post_root = ?artifact.boot_info.l2PostRoot,
        l2_block     = artifact.boot_info.l2BlockNumber,
        doc_bytes    = artifact.attestation_doc.len(),
        "received artifact"
    );

    let expected_user_data = range_user_data(&artifact.boot_info);
    parse_and_check_pcrs(
        &artifact.attestation_doc,
        &expected_pcrs,
        &expected_user_data,
    )?;

    tracing::info!("attestation verified OK");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("test-attestation is only supported on Linux");
}
