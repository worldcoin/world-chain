use alloy_eips::Encodable2718;
use alloy_primitives::Address;
use alloy_primitives::{Bytes, TxKind};
use alloy_provider::network::{EthereumWallet, TransactionBuilder};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_client::RpcClient;
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::SolCall;
use alloy_sol_types::SolValue;
use alloy_transport_http::Http;
use futures::{stream, StreamExt, TryStreamExt};
use reqwest::Client;
use semaphore_rs::{hash_to_field, identity::Identity};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tracing::info;
use world_chain_builder_pbh::{
    date_marker::DateMarker,
    external_nullifier::{EncodedExternalNullifier, ExternalNullifier},
    payload::PBHPayload,
};
use world_chain_builder_test_utils::bindings::IEntryPoint::PackedUserOperation;
use world_chain_builder_test_utils::bindings::{IMulticall3, IPBHEntryPoint};
use world_chain_builder_test_utils::utils::{user_op_sepolia, InclusionProof};

use super::SendArgs;
use super::{identities::SerializableIdentity, BundleArgs, TxType};

#[derive(Serialize, Deserialize, Debug)]
pub struct Bundle {
    pub pbh_transactions: Vec<Bytes>,
    pub pbh_user_operations: Vec<PackedUserOperation>,
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
    let std_transactions = bundle_std_transactions(&args).await?;

    match args.tx_type {
        TxType::Transaction => {
            let pbh_transactions = bundle_pbh_transactions(&args, identities).await?;
            serde_json::to_writer(
                std::fs::File::create(&args.bundle_path)?,
                &Bundle {
                    pbh_transactions,
                    pbh_user_operations: vec![],
                    std_transactions,
                },
            )?;
        }
        TxType::UserOperation => {
            let pbh_user_operations = bundle_pbh_user_operations(&args, identities).await?;
            serde_json::to_writer(
                std::fs::File::create(&args.bundle_path)?,
                &Bundle {
                    pbh_transactions: vec![],
                    pbh_user_operations,
                    std_transactions,
                },
            )?;
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

    let mut txs = vec![];
    let mut nonce = args.pbh_nonce;
    for (identity, proof) in identities.iter().zip(proofs.iter()) {
        let signer = PrivateKeySigner::from_str(&args.pbh_private_key)?;
        let sender = signer.address();
        let date = chrono::Utc::now().naive_utc().date();
        let date_marker = DateMarker::from(date);
        for i in 0..args.pbh_batch_size {
            let external_nullifier = ExternalNullifier::with_date_marker(date_marker, i);
            let external_nullifier_hash = EncodedExternalNullifier::from(external_nullifier).0;

            let call = IMulticall3::Call3::default();
            let calls = vec![call];
            let signal_hash = hash_to_field(&SolValue::abi_encode_packed(&(sender, calls.clone())));

            let root = proof.root;

            let semaphore_proof = semaphore_rs::protocol::generate_proof(
                identity,
                &proof.proof,
                external_nullifier_hash,
                signal_hash,
            )?;

            let nullifier_hash =
                semaphore_rs::protocol::generate_nullifier_hash(&identity, external_nullifier_hash);

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
                nonce: Some(nonce),
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

            nonce += 1;
            txs.push(sign_transaction(tx, signer.clone()).await?)
        }
    }

    Ok(txs)
}

pub async fn bundle_pbh_user_operations(
    args: &BundleArgs,
    identities: Vec<Identity>,
) -> eyre::Result<Vec<PackedUserOperation>> {
    let proofs = futures::future::try_join_all(
        identities
            .iter()
            .map(|identity| async { fetch_inclusion_proof(&args.sequencer_url, identity).await }),
    )
    .await?;

    let mut txs = vec![];
    for (identity, proof) in identities.iter().zip(proofs.iter()) {
        let signer = PrivateKeySigner::from_str(&args.pbh_private_key)?;
        let date = chrono::Utc::now().naive_utc().date();
        let date_marker = DateMarker::from(date);
        for i in 0..args.pbh_batch_size {
            let external_nullifier = ExternalNullifier::with_date_marker(date_marker, i);
            let uo = user_op_sepolia()
                .signer(signer.clone())
                .safe(args.user_op_args.safe.parse().expect("Invalid address"))
                .module(args.user_op_args.module.parse().expect("Invalid address"))
                .external_nullifier(external_nullifier)
                .inclusion_proof(proof.clone())
                .identity(identity.clone())
                .call();

            txs.push(uo);
        }
    }

    Ok(txs)
}

pub async fn bundle_std_transactions(args: &BundleArgs) -> eyre::Result<Vec<Bytes>> {
    let signer = args.std_private_key.parse::<PrivateKeySigner>()?;
    let sender = signer.address();
    let mut txs = vec![];
    let mut nonce = args.std_nonce;
    for _ in 0..args.tx_batch_size {
        let tx = TransactionRequest {
            nonce: Some(nonce),
            value: None,
            to: Some(TxKind::Call(Address::random())),
            gas: Some(100000),
            max_fee_per_gas: Some(20e10 as u128),
            max_priority_fee_per_gas: Some(20e10 as u128),
            chain_id: Some(args.chain_id),
            input: TransactionInput {
                input: None,
                data: Some(vec![0x00].into()),
            },
            from: Some(sender),
            ..Default::default()
        };

        nonce += 1;
        txs.push(sign_transaction(tx, signer.clone()).await?)
    }

    Ok(txs)
}

pub async fn sign_transaction(
    tx: TransactionRequest,
    signer: PrivateKeySigner,
) -> eyre::Result<Bytes> {
    let envelope = tx.build::<EthereumWallet>(&signer.into()).await?;
    Ok(envelope.encoded_2718().into())
}

pub async fn send_bundle(args: SendArgs) -> eyre::Result<()> {
    match args.tx_type {
        TxType::Transaction => {
            let bundle: Bundle = serde_json::from_reader(std::fs::File::open(&args.bundle_path)?)?;
            // TODO: Implement this
            // let mut headers = HeaderMap::new();
            // headers.insert(AUTHORIZATION, secret_to_bearer_header(&secret));

            // Create the reqwest::Client with the AUTHORIZATION header.
            let client_with_auth = Client::builder().build()?;

            // Create the HTTP transport.
            let http = Http::with_client(client_with_auth, args.rpc_url.parse()?);
            let rpc_client = RpcClient::new(http, false);
            let provider = ProviderBuilder::new().on_client(rpc_client);

            let txs = bundle
                .pbh_transactions
                .iter()
                .zip(bundle.std_transactions.iter());
            stream::iter(txs)
                .map(Ok)
                .try_for_each_concurrent(1000, |(pbh_tx, tx)| {
                    let provider = provider.clone();
                    async move {
                        let (res0, res1) = tokio::join!(
                            provider.send_raw_transaction(&pbh_tx.0),
                            provider.send_raw_transaction(&tx.0)
                        );
                        let res0 = res0?;
                        let res1 = res1?;
                        let hash_0 = res0.tx_hash();
                        let hash_1 = res1.tx_hash();

                        info!(?hash_0, "Sending PBH transaction");
                        info!(?hash_1, "Sending transaction");

                        let receipt_0 = res0.get_receipt().await?;
                        let receipt_1 = res1.get_receipt().await?;

                        info!(?receipt_0, "Received tx receipt for PBH Transaction");
                        info!(?receipt_1, "Received tx receipt for Non-PBH Transaction");
                        Ok::<_, eyre::Report>(())
                    }
                })
                .await?;
        }
        TxType::UserOperation => {}
    }
    Ok(())
}

async fn fetch_inclusion_proof(url: &str, identity: &Identity) -> eyre::Result<InclusionProof> {
    let client = reqwest::Client::new();

    let commitment = identity.commitment();
    let response = client
        .post(format!("{}/inclusionProof", url))
        .json(&serde_json::json! {{
            "identityCommitment": commitment,
        }})
        .send()
        .await?
        .error_for_status()?;

    let proof: InclusionProof = response.json().await?;

    Ok(proof)
}
