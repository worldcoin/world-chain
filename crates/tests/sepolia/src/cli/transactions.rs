use alloy_eips::Encodable2718;
use alloy_primitives::{Address, B256, U128, U256};
use alloy_primitives::{Bytes, TxKind};
use alloy_provider::network::{EthereumWallet, TransactionBuilder};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_client::RpcClient;
use alloy_rpc_types_eth::{BlockNumberOrTag, TransactionInput, TransactionRequest};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{sol, SolCall};
use alloy_sol_types::{SolInterface, SolValue};
use alloy_transport_http::Http;
use eyre::eyre::Context;
use futures::{stream, StreamExt, TryStreamExt};
use rand::Rng;
use reqwest::header::{HeaderMap, AUTHORIZATION};
use reqwest::Client;
use reth_rpc_layer::secret_to_bearer_header;
use semaphore_rs::{hash_to_field, identity::Identity};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::str::FromStr;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, info};
use world_chain_builder_pbh::{
    date_marker::DateMarker,
    external_nullifier::{EncodedExternalNullifier, ExternalNullifier},
    payload::PBHPayload,
};
use world_chain_builder_test_utils::bindings::IEntryPoint::PackedUserOperation;
use world_chain_builder_test_utils::bindings::{IMulticall3, IPBHEntryPoint};
use world_chain_builder_test_utils::utils::{
    partial_user_op_sepolia, user_op_sepolia, InclusionProof, RpcGasEstimate,
    RpcUserOperationByHash, RpcUserOperationV0_7,
};
use world_chain_builder_test_utils::DEVNET_ENTRYPOINT;

use crate::PBH_SIGNATURE_AGGREGATOR;

use super::{identities::SerializableIdentity, BundleArgs, TxType};
use super::{SendAAArgs, SendArgs};

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

    let std_transactions = bundle_transactions(&args).await?;

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
        for i in args.pbh_nonce..args.pbh_batch_size as u64 + args.pbh_nonce {
            let external_nullifier = ExternalNullifier::with_date_marker(date_marker, i as u16);
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
                semaphore_rs::protocol::generate_nullifier_hash(identity, external_nullifier_hash);

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
        for i in args.pbh_nonce..args.pbh_batch_size as u64 + args.pbh_nonce {
            let external_nullifier = ExternalNullifier::with_date_marker(date_marker, i as u16);
            let uo = user_op_sepolia()
                .signer(signer.clone())
                .safe(args.safe.expect("Safe address is required"))
                .module(args.module.expect("Module address is required"))
                .external_nullifier(external_nullifier)
                .inclusion_proof(proof.clone())
                .identity(identity.clone())
                .call();

            txs.push(uo);
        }
    }

    Ok(txs)
}

pub async fn bundle_transactions(args: &BundleArgs) -> eyre::Result<Vec<Bytes>> {
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
    let bundle: Bundle = serde_json::from_reader(std::fs::File::open(&args.bundle_path)?)?;

    let mut headers = HeaderMap::new();
    if let Some(secret) = args.auth {
        headers.insert(AUTHORIZATION, secret_to_bearer_header(&secret));
    }

    let client = Client::builder().default_headers(headers).build()?;

    // Create the HTTP transport.
    let http = Http::with_client(client, args.rpc_url.parse()?);
    let rpc_client = RpcClient::new(http, false);
    let provider = ProviderBuilder::new().on_client(rpc_client);
    match args.tx_type {
        TxType::Transaction => {
            stream::iter(
                bundle
                    .pbh_transactions
                    .iter()
                    .zip(bundle.std_transactions.iter()),
            )
            .map(Ok)
            .try_for_each_concurrent(1000, |(pbh_tx, tx)| {
                let provider = provider.clone();
                async move {
                    let (response_pbh, response_tx) = tokio::join!(
                        provider.send_raw_transaction(&pbh_tx.0),
                        provider.send_raw_transaction(&tx.0)
                    );
                    let pbh_builder = response_pbh?;
                    let tx_builder = response_tx?;

                    let pbh_hash = pbh_builder.tx_hash();
                    let tx_hash = tx_builder.tx_hash();

                    info!(?pbh_hash, "Sending PBH transaction");
                    info!(?tx_hash, "Sending transaction");

                    let pbh_receipt = pbh_builder.get_receipt().await?;
                    let tx_receipt = tx_builder.get_receipt().await?;
                    debug!(?pbh_receipt, ?tx_receipt, "Receipts");

                    Ok::<_, eyre::Report>(())
                }
            })
            .await?;
        }
        TxType::UserOperation => {
            stream::iter(bundle.pbh_user_operations.iter())
                .map(Ok)
                .try_for_each_concurrent(1000, move |uo| {
                    let provider = provider.clone();
                    async move {
                        let uo: RpcUserOperationV0_7 = (uo.clone(), PBH_SIGNATURE_AGGREGATOR).into();
                        let hash: B256 = provider.raw_request(
                            Cow::Borrowed("eth_sendUserOperation"),
                            (uo, DEVNET_ENTRYPOINT),
                        )
                        .await?;

                        // Fetch the Transaction by hash
                        let max_retries = 100;
                        let mut tries = 0;
                        loop {
                            if tries >= max_retries {
                                panic!("User Operation not included in a Transaction after {} retries", max_retries);
                            }
                            // Check if the User Operation has been included in a Transaction
                            let resp: RpcUserOperationByHash = provider
                                .raw_request(
                                    Cow::Borrowed("eth_getUserOperationByHash"),
                                    (hash,),
                                )
                                .await?;

                            if let Some(transaction_hash) = resp.transaction_hash {
                                // Fetch the Transaction Receipt from the builder
                                let receipt = provider.get_transaction_by_hash(transaction_hash).await?;
                                assert!(receipt.is_some_and(|receipt| {
                                    debug!(target: "tests::user_ops_test",  ?receipt, "Transaction Receipt Received");
                                    true
                                }));

                                break;
                            }

                            tries += 1;
                            sleep(Duration::from_secs(2)).await;
                        }
                        Ok::<(), eyre::Report>(())
                    }
                }).await?;
        }
    }
    Ok(())
}

sol! {
    contract EntryPoint {
        function addStake(uint32 unstakeDelaySec) public payable;
    }

    contract Safe {
        function executeUserOp(address to, uint256 value, bytes calldata data, uint8 operation) external;
    }
}

pub async fn send_aa(args: SendAAArgs) -> eyre::Result<()> {
    let identities: Vec<SerializableIdentity> =
        serde_json::from_reader(std::fs::File::open(&args.identities_path)?)?;
    let identities = identities
        .into_iter()
        .map(|identity| Identity {
            nullifier: identity.nullifier,
            trapdoor: identity.trapdoor,
        })
        .collect::<Vec<_>>();

    let proofs = futures::future::try_join_all(
        identities
            .iter()
            .map(|identity| async { fetch_inclusion_proof(&args.sequencer_url, identity).await }),
    )
    .await?;

    let provider = ProviderBuilder::new().connect(&args.rpc_url).await?;

    // calldata for addStake
    // let inner_calldata: Bytes = EntryPoint::EntryPointCalls::addStake(EntryPoint::addStakeCall {
    //     unstakeDelaySec: 86400,
    // })
    // .abi_encode()
    // .into();

    // let calldata: Bytes = Safe::SafeCalls::executeUserOp(Safe::executeUserOpCall {
    //     to: DEVNET_ENTRYPOINT,
    //     value: alloy_primitives::uint!(100000000000000000_U256),
    //     data: inner_calldata,
    //     operation: 0,
    // })
    // .abi_encode()
    // .into();

    // empty calldata
    let calldata: Bytes = Safe::SafeCalls::executeUserOp(Safe::executeUserOpCall {
        to: Address::ZERO,
        value: U256::ZERO,
        data: Bytes::new(),
        operation: 0,
    })
    .abi_encode()
    .into();

    let puo = partial_user_op_sepolia()
        .safe(args.safe)
        .calldata(calldata.clone())
        .call();

    let resp: RpcGasEstimate = provider
        .raw_request(
            Cow::Borrowed("eth_estimateUserOperationGas"),
            (puo, DEVNET_ENTRYPOINT),
        )
        .await?;

    info!("Estimated gas: {resp:?}");

    // let base_fee = provider
    //     .get_fee_history(1, BlockNumberOrTag::Latest, &[])
    //     .await
    //     .context("Failed to get fee history")?
    //     .next_block_base_fee()
    //     .expect("Failed to get base fee");

    let base_fee = 250_u128;

    let priority_fee: U128 = provider
        .raw_request(Cow::Borrowed("rundler_maxPriorityFeePerGas"), ())
        .await?;
    let max_fee = U128::from(base_fee * 2) + priority_fee * U128::from(3) / U128::from(2);
    let fees = concat_u128_be(priority_fee, max_fee);

    let account_gas_limits = concat_u128_be(
        resp.verification_gas_limit.into(),
        resp.call_gas_limit.into(),
    );

    let mut uos = vec![];
    for (identity, proof) in identities.iter().zip(proofs.iter()) {
        let signer = PrivateKeySigner::from_str(&args.pbh_private_key)?;
        let date = chrono::Utc::now().naive_utc().date();
        let date_marker = DateMarker::from(date);
        let pbh_nonce = rand::thread_rng().gen_range(0..u16::MAX) as u16;
        let pbh_batch_size = args.pbh_batch_size as u16;

        for i in pbh_nonce..pbh_batch_size + pbh_nonce {
            let external_nullifier = ExternalNullifier::with_date_marker(date_marker, i);

            let uo = user_op_sepolia()
                .signer(signer.clone())
                .safe(args.safe)
                .module(args.module)
                .external_nullifier(external_nullifier)
                .inclusion_proof(proof.clone())
                .identity(identity.clone())
                .pre_verification_gas(U256::from(
                    resp.pre_verification_gas * U128::from(5) / U128::from(4),
                ))
                .account_gas_limits(account_gas_limits.into())
                .gas_fees(fees.into())
                .calldata(calldata.clone())
                .call();

            uos.push(uo);
        }
    }

    for uo in uos {
        let uo: RpcUserOperationV0_7 = (uo.clone(), PBH_SIGNATURE_AGGREGATOR).into();
        let hash: B256 = provider
            .raw_request(
                Cow::Borrowed("eth_sendUserOperation"),
                (uo, DEVNET_ENTRYPOINT),
            )
            .await?;

        info!(?hash, "Sending User Operation");
    }

    Ok(())
}

fn concat_u128_be(a: U128, b: U128) -> [u8; 32] {
    let a: [u8; 16] = a.to_be_bytes();
    let b: [u8; 16] = b.to_be_bytes();
    std::array::from_fn(|i| {
        if let Some(i) = i.checked_sub(a.len()) {
            b[i]
        } else {
            a[i]
        }
    })
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
