use alloy_eips::Encodable2718;
use alloy_primitives::{address, bytes, fixed_bytes, Address, FixedBytes, B256, U128, U256};
use alloy_primitives::{Bytes, TxKind};
use alloy_provider::network::{EthereumWallet, TransactionBuilder};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_client::RpcClient;
use alloy_rpc_types_eth::{BlockNumberOrTag, TransactionInput, TransactionRequest};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{sol, SolCall};
use alloy_sol_types::{SolInterface, SolValue};
use alloy_transport_http::Http;
use eyre::eyre::{bail, Context};
use futures::{stream, StreamExt, TryStreamExt};
use rand::Rng;
use reqwest::header::{HeaderMap, AUTHORIZATION};
use reqwest::Client;
use reth_rpc_layer::secret_to_bearer_header;
use semaphore_rs::{hash_to_field, identity::Identity};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::sleep;
use tracing::{debug, error, info};
use world_chain_builder_pbh::{
    date_marker::DateMarker,
    external_nullifier::{EncodedExternalNullifier, ExternalNullifier},
    payload::PBHPayload,
};
use world_chain_builder_test_utils::bindings::IEntryPoint::{
    PackedUserOperation, UserOpsPerAggregator,
};
use world_chain_builder_test_utils::bindings::{IMulticall3, IPBHEntryPoint};
use world_chain_builder_test_utils::utils::{
    get_operation_hash, partial_user_op_sepolia, user_op_sepolia, InclusionProof, RpcGasEstimate,
    RpcPartialUserOperation, RpcUserOperationByHash, RpcUserOperationV0_7,
};
use world_chain_builder_test_utils::{DEVNET_ENTRYPOINT, WC_SEPOLIA_CHAIN_ID};

use crate::PBH_SIGNATURE_AGGREGATOR;

use super::{identities::SerializableIdentity, BundleArgs, TxType};
use super::{SendAAArgs, SendArgs, SendInvalidProofPBHArgs, StakeAAArgs};
use world_chain_builder_test_utils::bindings::IPBHEntryPoint::PBHPayload as PBHPayloadSolidity;

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
    let provider = ProviderBuilder::new().connect_client(rpc_client);
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
                                panic!("User Operation not included in a Transaction after {max_retries} retries");
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

pub async fn stake_aa(args: StakeAAArgs) -> eyre::Result<()> {
    // calldata for addStake
    let inner_calldata: Bytes = EntryPoint::EntryPointCalls::addStake(EntryPoint::addStakeCall {
        unstakeDelaySec: 86400,
    })
    .abi_encode()
    .into();

    let calldata: Bytes = Safe::SafeCalls::executeUserOp(Safe::executeUserOpCall {
        to: DEVNET_ENTRYPOINT,
        value: args.stake_amount,
        data: inner_calldata,
        operation: 0,
    })
    .abi_encode()
    .into();

    let signer = PrivateKeySigner::from_str(&args.pbh_private_key)?;

    let puo = partial_user_op_sepolia()
        .safe(args.safe)
        .calldata(calldata.clone())
        .call();

    let provider = Arc::new(ProviderBuilder::new().connect(&args.rpc_url).await?);

    let (account_gas_limits, fees, pre_verification_gas) =
        estimate_uo_gas(provider.clone(), &puo).await?;

    let uo = user_op_sepolia()
        .signer(signer)
        .safe(args.safe)
        .module(args.module)
        .pre_verification_gas(U256::from(
            pre_verification_gas * U128::from(5) / U128::from(4),
        ))
        .account_gas_limits(account_gas_limits)
        .gas_fees(fees)
        .calldata(calldata)
        .call();

    let rpc_uo: RpcUserOperationV0_7 = uo.into();

    let hash: B256 = provider
        .raw_request(
            Cow::Borrowed("eth_sendUserOperation"),
            (rpc_uo, DEVNET_ENTRYPOINT),
        )
        .await
        .context("Failed to send User Operation")?;

    let max_retries = 1000;
    let mut i = 0;
    while i < max_retries {
        let resp: Option<RpcUserOperationByHash> = provider
            .raw_request(Cow::Borrowed("eth_getUserOperationByHash"), (hash,))
            .await
            .context("Failed to get User Operation by hash")?;

        let Some(resp) = resp else {
            bail!("UO {hash:?} dropped");
        };

        let Some(transaction_hash) = resp.transaction_hash else {
            i += 1;
            sleep(Duration::from_millis(100)).await;
            continue;
        };

        debug!("UO {hash:?} included in transaction {transaction_hash:?}");
        return Ok(());
    }

    bail!("UO {hash:?} not included in any transaction after {max_retries} retries");
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

    let provider = Arc::new(ProviderBuilder::new().connect(&args.rpc_url).await?);

    // empty calldata
    let calldata: Bytes = Safe::SafeCalls::executeUserOp(Safe::executeUserOpCall {
        to: Address::ZERO,
        value: U256::ZERO,
        data: Bytes::new(),
        operation: 0,
    })
    .abi_encode()
    .into();

    let pbh_nonce = args
        .pbh_nonce
        .map(|n| n as u16)
        .unwrap_or_else(|| rand::rng().random_range(0..u16::MAX));
    let signer = PrivateKeySigner::from_str(&args.pbh_private_key)?;
    let date = chrono::Utc::now().naive_utc().date();
    let date_marker = DateMarker::from(date);

    let semaphore = Arc::new(Semaphore::new(args.concurrency));
    let total = args.pbh_batch_size as usize * identities.len();

    for i in 0..total {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("Failed to acquire semaphore");

        let identity = &identities[i % identities.len()];
        let proof = &proofs[i % proofs.len()];
        let round = i / identities.len();

        info!(
            "Sending User Operation {} in of {} in round {}",
            i,
            total - 1,
            round
        );

        let external_nullifier =
            ExternalNullifier::with_date_marker(date_marker, pbh_nonce + round as u16);

        let provider = provider.clone();
        let signer = signer.clone();
        let calldata = calldata.clone();
        let proof = proof.clone();
        let identity = identity.clone();

        tokio::spawn(async move {
            send_uo_task(
                i,
                provider,
                permit,
                signer,
                args.safe,
                args.module,
                external_nullifier,
                proof,
                identity,
                calldata,
            )
            .await;
        });
    }

    // Wait for all the User Operations to be mined
    let _ = semaphore
        .acquire_many(args.concurrency as u32)
        .await
        .expect("Failed to acquire semaphore");

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn send_uo_task(
    index: usize,
    provider: Arc<impl Provider>,
    _permit: OwnedSemaphorePermit,
    signer: PrivateKeySigner,
    safe: Address,
    module: Address,
    external_nullifier: ExternalNullifier,
    proof: InclusionProof,
    identity: Identity,
    calldata: Bytes,
) {
    if let Err(e) = send_uo_task_inner(
        provider,
        signer,
        safe,
        module,
        external_nullifier,
        proof,
        identity,
        calldata,
    )
    .await
    {
        error!("UO {index} failed: {e}");
    }
}

#[allow(clippy::too_many_arguments)]
async fn send_uo_task_inner(
    provider: Arc<impl Provider>,
    signer: PrivateKeySigner,
    safe: Address,
    module: Address,
    external_nullifier: ExternalNullifier,
    proof: InclusionProof,
    identity: Identity,
    calldata: Bytes,
) -> eyre::Result<()> {
    let puo: RpcPartialUserOperation = partial_user_op_sepolia()
        .safe(safe)
        .calldata(calldata.clone())
        .call();

    let (account_gas_limits, fees, pre_verification_gas) =
        estimate_uo_gas(provider.clone(), &puo).await?;

    let uo: PackedUserOperation = user_op_sepolia()
        .signer(signer)
        .safe(safe)
        .module(module)
        .external_nullifier(external_nullifier)
        .inclusion_proof(proof)
        .identity(identity)
        .pre_verification_gas(U256::from(
            pre_verification_gas * U128::from(5) / U128::from(4),
        ))
        .account_gas_limits(account_gas_limits)
        .gas_fees(fees)
        .calldata(calldata)
        .call();

    let rpc_uo: RpcUserOperationV0_7 = (uo.clone(), PBH_SIGNATURE_AGGREGATOR).into();

    let hash: B256 = provider
        .raw_request(
            Cow::Borrowed("eth_sendUserOperation"),
            (rpc_uo, DEVNET_ENTRYPOINT),
        )
        .await
        .context("Failed to send User Operation")?;

    let max_retries = 1000;
    let mut i = 0;
    while i < max_retries {
        let resp: Option<RpcUserOperationByHash> = provider
            .raw_request(Cow::Borrowed("eth_getUserOperationByHash"), (hash,))
            .await
            .context("Failed to get User Operation by hash")?;

        let Some(resp) = resp else {
            bail!("UO {hash:?} dropped");
        };

        let Some(transaction_hash) = resp.transaction_hash else {
            i += 1;
            sleep(Duration::from_millis(100)).await;
            continue;
        };

        debug!("UO {hash:?} included in transaction {transaction_hash:?}");
        return Ok(());
    }

    bail!("UO {hash:?} not included in any transaction after {max_retries} retries");
}

async fn estimate_uo_gas(
    provider: impl Provider,
    puo: &RpcPartialUserOperation,
) -> eyre::Result<(FixedBytes<32>, FixedBytes<32>, U128)> {
    let resp: RpcGasEstimate = provider
        .raw_request(
            Cow::Borrowed("eth_estimateUserOperationGas"),
            (puo, DEVNET_ENTRYPOINT),
        )
        .await?;

    debug!("Estimated gas: {resp:?}");

    let base_fee = provider
        .get_fee_history(1, BlockNumberOrTag::Latest, &[])
        .await
        .context("Failed to get fee history")?
        .next_block_base_fee()
        .expect("Failed to get base fee");

    let priority_fee: U128 = provider
        .raw_request(Cow::Borrowed("rundler_maxPriorityFeePerGas"), ())
        .await?;
    let max_fee = U128::from(base_fee * 2) + priority_fee * U128::from(3) / U128::from(2);
    let fees = concat_u128_be(priority_fee, max_fee);

    let account_gas_limits = concat_u128_be(resp.verification_gas_limit, resp.call_gas_limit);

    Ok((
        account_gas_limits.into(),
        fees.into(),
        resp.pre_verification_gas,
    ))
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
        .post(format!("{url}/inclusionProof"))
        .json(&serde_json::json! {{
            "identityCommitment": commitment,
        }})
        .send()
        .await?
        .error_for_status()?;

    let proof: InclusionProof = response.json().await?;

    Ok(proof)
}

pub async fn send_invalid_pbh(args: SendInvalidProofPBHArgs) -> eyre::Result<()> {
    let wallet = EthereumWallet::new(args.pbh_private_key.parse::<PrivateKeySigner>()?);
    let provider = Arc::new(
        ProviderBuilder::new()
            .wallet(wallet)
            .connect(&args.rpc_url)
            .await?,
    );

    // empty calldata
    let calldata: Bytes = Safe::SafeCalls::executeUserOp(Safe::executeUserOpCall {
        to: Address::ZERO,
        value: U256::ZERO,
        data: Bytes::new(),
        operation: 0,
    })
    .abi_encode()
    .into();

    let pbh_nonce = args
        .pbh_nonce
        .map(|n| n as u16)
        .unwrap_or_else(|| rand::rng().random_range(0..u16::MAX));
    let signer = PrivateKeySigner::from_str(&args.pbh_private_key)?;
    let date = chrono::Utc::now().naive_utc().date();
    let date_marker = DateMarker::from(date);

    debug!("Starting pbh_nonce: {pbh_nonce}");

    let mut i = 0;
    while i < args.transaction_count {
        let current_pbh_nonce = pbh_nonce + i as u16;
        let external_nullifier =
            ExternalNullifier::with_date_marker(date_marker, current_pbh_nonce);

        let provider = provider.clone();
        let signer = signer.clone();
        let calldata = calldata.clone();

        let puo: RpcPartialUserOperation = partial_user_op_sepolia()
            .safe(args.safe)
            .calldata(calldata.clone())
            .call();

        let (account_gas_limits, _fees, _pre_verification_gas) =
            estimate_uo_gas(provider.clone(), &puo).await?;

        let rand_key = U256::from_be_bytes(Address::random().into_word().0) << 32;
        let nonce_key = U256::from(1123123123);

        let mut user_op = PackedUserOperation {
            sender: args.safe,
            nonce: ((rand_key | nonce_key) << 64) | U256::from(0),
            initCode: Bytes::default(),
            callData: calldata,
            accountGasLimits: account_gas_limits,
            preVerificationGas: U256::from(500836),
            gasFees: fixed_bytes!(
                "0000000000000000000000003B9ACA0000000000000000000000000073140B60"
            ),
            paymasterAndData: Bytes::default(),
            signature: bytes!("000000000000000000000000"),
        };

        let operation_hash = get_operation_hash(user_op.clone(), args.module, WC_SEPOLIA_CHAIN_ID);

        let signature = signer
            .sign_message_sync(&operation_hash.0)
            .expect("Failed to sign operation hash");

        let pbh_payload: PBHPayload = PBHPayload {
            external_nullifier,
            ..Default::default()
        };

        let mut uo_sig = Vec::new();

        // https://github.com/safe-global/safe-smart-account/blob/21dc82410445637820f600c7399a804ad55841d5/contracts/Safe.sol#L323
        let v: FixedBytes<1> = if signature.v() as u8 == 0 {
            fixed_bytes!("1F") // 31
        } else {
            fixed_bytes!("20") // 32
        };

        uo_sig.extend_from_slice(
            &(
                fixed_bytes!("000000000000000000000000"),
                signature.r(),
                signature.s(),
                v,
            )
                .abi_encode_packed(),
        );

        user_op.signature = Bytes::from(uo_sig);

        let bundle = IPBHEntryPoint::handleAggregatedOpsCall {
            _0: vec![UserOpsPerAggregator {
                userOps: vec![user_op],
                signature: vec![PBHPayloadSolidity::from(pbh_payload)]
                    .abi_encode()
                    .into(),
                aggregator: PBH_SIGNATURE_AGGREGATOR,
            }],
            _1: address!("0x6348A4a4dF173F68eB28A452Ca6c13493e447aF1"),
        };

        let encoded = bundle.abi_encode();
        let encoded_bytes = Bytes::from(encoded.clone()).to_string();

        println!("Encoded: {encoded_bytes}");

        let tx = TransactionRequest {
            to: Some(TxKind::Call(args.pbh_entry_point.parse()?)),
            input: TransactionInput {
                input: Some(Bytes::from(encoded)),
                data: None,
            },
            ..Default::default()
        };

        // This should revert on builder PBH validation error if the builders are up
        // If all the builders are down/tx-proxy is down, the tx will be mined as the relays simulate the tx without valdiating the proof
        _ = provider.send_transaction(tx).await;

        // println!("Tx hash: {tx_hash:?}");

        // println!("Tx hash: {tx_hash:?}");

        i += 1;
    }

    Ok(())
}
