use alloy_eips::Encodable2718;
use alloy_primitives::{Bytes, TxKind};
use alloy_provider::network::{EthereumWallet, TransactionBuilder};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::SolCall;
use alloy_sol_types::SolValue;
use semaphore_rs::{hash_to_field, identity::Identity, poseidon_tree::Proof, Field};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tokio::runtime::Handle;
use world_chain_builder_pbh::{
    date_marker::DateMarker,
    external_nullifier::{EncodedExternalNullifier, ExternalNullifier},
    payload::PBHPayload,
};
use world_chain_builder_test_utils::bindings::{IMulticall3, IPBHEntryPoint};

use super::{identities::SerializableIdentity, BundleArgs, TxType};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InclusionProof {
    root: Field,
    proof: Proof,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Bundle {
    pub pbh_transactions: Vec<Bytes>,
    pub std_transactions: Vec<Bytes>,
}

pub async fn create_bundle(args: BundleArgs) -> eyre::Result<()> {
    let identities: Vec<SerializableIdentity> =
        serde_json::from_reader(std::fs::File::open(&args.identities_path)?)?;
    let identities = identities
        .into_iter()
        .map(|identity| Identity {
            nullifier: identity.nullifier,
            trapdoor: identity.trapdoor,
        })
        .collect();
    match args.tx_type {
        TxType::Transaction => {
            let pbh_transactions = bundle_pbh_transactions(&args, identities).await?;
            let std_transactions = create_std_transactions(&args)?;
            serde_json::to_writer(
                std::fs::File::create(&args.bundle_path)?,
                &Bundle {
                    pbh_transactions,
                    std_transactions,
                },
            )?;
        }
        TxType::UserOperation => {
            bundle_pbh_user_operations(&args, identities)?;
        }
    }

    Ok(())
}

pub async fn bundle_pbh_transactions(
    args: &BundleArgs,
    identities: Vec<Identity>,
) -> eyre::Result<Vec<Bytes>> {
    let proofs = futures::future::try_join_all(
        identities
            .iter()
            .map(|identity| async { fetch_inclusion_proof(&args.sequencer_url, identity).await }),
    )
    .await?;

    let txs = identities
        .iter()
        .zip(proofs.iter())
        .map(|(identity, proof)| {
            let signer = PrivateKeySigner::from_str(&args.private_key)?;
            let sender = signer.address();
            let date = chrono::Utc::now().naive_utc().date();
            let date_marker = DateMarker::from(date);
            let mut transactions: Vec<Bytes> = vec![];
            for i in 0..args.pbh_batch_size {
                let external_nullifier = ExternalNullifier::with_date_marker(date_marker, i);
                let external_nullifier_hash = EncodedExternalNullifier::from(external_nullifier).0;

                let call = IMulticall3::Call3::default();
                let calls = vec![call];
                let signal_hash =
                    hash_to_field(&SolValue::abi_encode_packed(&(sender, calls.clone())));

                let root = proof.root;

                let semaphore_proof = semaphore_rs::protocol::generate_proof(
                    identity,
                    &proof.proof,
                    external_nullifier_hash,
                    signal_hash,
                )?;

                let nullifier_hash = semaphore_rs::protocol::generate_nullifier_hash(
                    &identity,
                    external_nullifier_hash,
                );

                let payload = PBHPayload {
                    root,
                    nullifier_hash,
                    external_nullifier,
                    proof: world_chain_builder_pbh::payload::Proof(semaphore_proof),
                };

                let calldata = IPBHEntryPoint::pbhMulticallCall {
                    calls,
                    payload: payload.into(),
                };
                let tx = TransactionRequest {
                    nonce: Some(args.nonce + i as u64),
                    value: None,
                    to: Some(TxKind::Call(
                        args.pbh_entry_point.parse().expect("Invalid address"),
                    )),
                    gas: Some(100000),
                    max_fee_per_gas: Some(20e10 as u128),
                    max_priority_fee_per_gas: Some(20e10 as u128),
                    chain_id: Some(args.chain_id),
                    input: TransactionInput {
                        input: None,
                        data: Some(calldata.abi_encode().into()),
                    },
                    from: Some(sender),
                    ..Default::default()
                };

                let rt = Handle::current();
                let envelope = rt.block_on(tx.build::<EthereumWallet>(&signer.clone().into()))?;
                transactions.push(envelope.encoded_2718().into());
            }

            Ok::<Vec<Bytes>, eyre::Report>(transactions)
        })
        .flatten()
        .flatten()
        .collect();

    Ok(txs)
}

// TODO:
pub fn bundle_pbh_user_operations(
    args: &BundleArgs,
    identities: Vec<Identity>,
) -> eyre::Result<()> {
    Ok(())
}

pub fn create_std_transactions(args: &BundleArgs) -> eyre::Result<Vec<Bytes>> {
    Ok(vec![])
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
