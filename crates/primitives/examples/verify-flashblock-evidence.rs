//! Verify a saved RPC result using a separately trusted key and expected immutable view.
use alloy_primitives::B256;
use eyre::eyre::ensure;
use std::{env, fs::File, io::Read};
use world_chain_primitives::{
    ed25519_dalek::VerifyingKey,
    evidence::{FlashblockEvidence, FlashblockEvidenceId, MAX_FLASHBLOCK_EVIDENCE_BYTES},
};

fn main() -> eyre::Result<()> {
    let args: Vec<_> = env::args().collect();
    ensure!(
        args.len() == 7,
        "usage: verify-flashblock-evidence RESULT.json AUTHORIZER_HEX CHAIN_ID PAYLOAD_ID INDEX BLOCK_HASH"
    );
    let mut bytes = Vec::new();
    File::open(&args[1])?
        .take((MAX_FLASHBLOCK_EVIDENCE_BYTES * 2 + 65537) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_FLASHBLOCK_EVIDENCE_BYTES * 2 + 65536,
        "evidence JSON exceeds size limit"
    );
    let evidence: FlashblockEvidence = serde_json::from_slice(&bytes)?;
    let authorizer = VerifyingKey::from_bytes(&args[2].parse::<B256>()?.0)?;
    let expected = FlashblockEvidenceId {
        payload_id: args[4].parse()?,
        index: args[5].parse()?,
        block_hash: args[6].parse()?,
    };
    evidence.verify(args[3].parse()?, authorizer, expected)?;
    println!(
        "Verified signed Flashblock origin for {} index {} ({})",
        expected.payload_id, expected.index, expected.block_hash
    );
    Ok(())
}
