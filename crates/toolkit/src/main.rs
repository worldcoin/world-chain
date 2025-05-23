use alloy_consensus::TxEnvelope;
use alloy_rlp::Decodable;
use clap::Parser;
use cli::inclusion_proof_source::InclusionProofSourceVariant;
use cli::{Cmd, Opt};
use semaphore_rs::identity::Identity;
use semaphore_rs::poseidon_tree::Proof;
use semaphore_rs::{hash_to_field, Field};
use serde::{Deserialize, Serialize};
use world_chain_builder_pbh::date_marker::DateMarker;
use world_chain_builder_pbh::external_nullifier::{EncodedExternalNullifier, ExternalNullifier};
use world_chain_builder_pbh::payload::PBHPayload;

mod cli;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InclusionProof {
    root: Field,
    proof: Proof,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    dotenvy::dotenv().ok();

    let args = Opt::parse();

    match args.cmd {
        Cmd::Prove(prove_args) => {
            let raw_tx_bytes = prove_args.tx;
            let tx: TxEnvelope = TxEnvelope::decode(&mut raw_tx_bytes.as_ref())?;

            let tx_hash = tx.tx_hash();
            let signal_hash = hash_to_field(tx_hash.as_ref());

            let identity = prove_args.identity_source.load();

            let inclusion_proof_proof_src =
                prove_args.inclusion_proof_source.clone().into_variant();
            let inclusion_proof = match inclusion_proof_proof_src {
                InclusionProofSourceVariant::Proof(proof) => proof,
                InclusionProofSourceVariant::File(file) => load_inclusion_proof_file(file)?,
                InclusionProofSourceVariant::Url(url) => {
                    fetch_inclusion_proof(&url, &identity).await?
                }
            };

            let date = prove_args
                .custom_date
                .unwrap_or_else(|| chrono::Utc::now().naive_utc().date());

            let date_marker = DateMarker::from(date);

            let external_nullifier =
                ExternalNullifier::with_date_marker(date_marker, prove_args.pbh_nonce as u16);
            let external_nullifier_hash = EncodedExternalNullifier::from(external_nullifier).0;

            let semaphore_proof = semaphore_rs::protocol::generate_proof(
                &identity,
                &inclusion_proof.proof,
                external_nullifier_hash,
                signal_hash,
            )?;

            let nullifier_hash =
                semaphore_rs::protocol::generate_nullifier_hash(&identity, external_nullifier_hash);

            let proof = PBHPayload {
                external_nullifier,
                nullifier_hash,
                root: inclusion_proof.root,
                proof: world_chain_builder_pbh::payload::Proof(semaphore_proof),
            };

            let encoded = alloy_rlp::encode(proof);

            let concatenated_bytes = [raw_tx_bytes.as_ref(), encoded.as_slice()].concat();

            let encoded_hex = hex::encode(concatenated_bytes);

            println!("{}", encoded_hex);
        }
    }

    Ok(())
}

fn load_inclusion_proof_file(path: impl AsRef<std::path::Path>) -> eyre::Result<InclusionProof> {
    let file = std::fs::File::open(path)?;
    let proof = serde_json::from_reader(file)?;

    Ok(proof)
}

pub async fn fetch_inclusion_proof(url: &str, identity: &Identity) -> eyre::Result<InclusionProof> {
    let client = reqwest::Client::new();

    let commitment = identity.commitment();
    let response = client
        .post(url)
        .json(&serde_json::json! {{
            "identityCommitment": commitment,
        }})
        .send()
        .await?
        .error_for_status()?;

    let proof: InclusionProof = response.json().await?;

    Ok(proof)
}
