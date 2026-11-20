//! Verify a claimed log or recover every matching log from a complete signed receipt view.

use std::{env, fs::File, io::Read};

use alloy_primitives::{Address, B256, Log};
use eyre::eyre::ensure;
use serde::de::DeserializeOwned;
use serde_json::json;
use world_chain_primitives::{
    ed25519_dalek::VerifyingKey,
    evidence::{FlashblockEvidence, FlashblockEvidenceId, MAX_FLASHBLOCK_EVIDENCE_BYTES},
    receipt_proof::{
        FlashblockReceiptProof, FlashblockReceipts, MAX_BUNDLE_JSON_BYTES, verify_log,
    },
};

fn main() -> eyre::Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    ensure!(
        args.len() >= 8,
        "usage: verify-flashblock-receipts log|complete EVIDENCE.json BUNDLE.json AUTHORIZER_HEX CHAIN_ID PAYLOAD_ID INDEX BLOCK_HASH [TX_HASH ORDINAL LOG.json | ADDRESS TOPIC0]"
    );
    let proof_mode = args[0] == "log" && args.len() == 11;
    let complete_mode = args[0] == "complete" && args.len() == 10;
    ensure!(
        proof_mode || complete_mode,
        "log requires TX_HASH ORDINAL LOG.json; complete requires ADDRESS TOPIC0"
    );
    let evidence: FlashblockEvidence = read(&args[1], MAX_FLASHBLOCK_EVIDENCE_BYTES * 2 + 65536)?;
    let authorizer = VerifyingKey::from_bytes(&args[3].parse::<B256>()?.0)?;
    let chain_id = args[4].parse()?;
    let id = FlashblockEvidenceId {
        payload_id: args[5].parse()?,
        index: args[6].parse()?,
        block_hash: args[7].parse()?,
    };
    if proof_mode {
        let proof: FlashblockReceiptProof = read(&args[2], MAX_BUNDLE_JSON_BYTES)?;
        let transaction_hash = args[8].parse()?;
        let ordinal = args[9].parse()?;
        let log: Log = read(&args[10], MAX_BUNDLE_JSON_BYTES)?;
        let receipt =
            proof.verify_receipt(&evidence, chain_id, authorizer, id, transaction_hash)?;
        verify_log(&receipt, ordinal, &log)?;
        println!(
            "Verified transaction {transaction_hash} and log ordinal {ordinal} in {}",
            id.block_hash
        );
    } else {
        let complete: FlashblockReceipts = read(&args[2], MAX_BUNDLE_JSON_BYTES)?;
        let address: Address = args[8].parse()?;
        let topic0: B256 = args[9].parse()?;
        let receipts = complete.verify(&evidence, chain_id, authorizer, id)?;
        for (transaction_index, receipt) in receipts.iter().enumerate() {
            for (ordinal, log) in receipt.logs().iter().enumerate() {
                if log.address == address && log.data.topics().first() == Some(&topic0) {
                    println!(
                        "{}",
                        json!({"transactionIndex": transaction_index, "ordinal": ordinal, "log": log})
                    );
                }
            }
        }
        eprintln!(
            "Verified {} complete receipts for {}",
            receipts.len(),
            id.block_hash
        );
    }
    Ok(())
}

fn read<T: DeserializeOwned>(path: &str, limit: usize) -> eyre::Result<T> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "JSON file {path} exceeds size limit");
    Ok(serde_json::from_slice(&bytes)?)
}
