use alloy_consensus::TxEnvelope;
use alloy_rlp::Decodable;
use clap::Parser;
use cli::{Cmd, Opt};
use semaphore::hash_to_field;
use world_chain_builder::date_marker::DateMarker;
use world_chain_builder::external_nullifier::ExternalNullifier;

mod cli;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    dotenvy::dotenv().ok();

    let args = Opt::parse();
    println!("{:#?}", args);

    match args.cmd {
        Cmd::Prove(prove_args) => {
            let tx: TxEnvelope = TxEnvelope::decode(&mut prove_args.tx.as_ref())?;

            let tx_hash = tx.tx_hash();
            let signal_hash = hash_to_field(tx_hash.as_ref());

            let identity = prove_args.identity_source.load();
            let merkle_proof = prove_args.inclusion_proof_source.load();

            let date = prove_args
                .custom_date
                .unwrap_or_else(|| chrono::Utc::now().naive_utc().date());

            let month = DateMarker::from(date);

            let external_nullifier = ExternalNullifier::new(month, prove_args.pbh_nonce);
            let external_nullifier_hash = external_nullifier.hash();

            let semaphore_proof = semaphore::protocol::generate_proof(
                &identity,
                &merkle_proof,
                external_nullifier_hash,
                signal_hash,
            )?;
        }
        _ => unimplemented!(),
    }

    Ok(())
}
