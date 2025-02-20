use alloy_consensus::TxEnvelope;
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{address, Address, Bytes, TxKind, U256};
use alloy_provider::network::NetworkWallet;
use alloy_provider::network::{Ethereum, EthereumWallet, TransactionBuilder};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::SolCall;
use alloy_sol_types::SolValue;
use semaphore_rs::{hash_to_field, identity::Identity};
use signup_sequencer::server::data::InclusionProofResponse;
use world_chain_builder_pbh::{
    date_marker::DateMarker,
    external_nullifier::{EncodedExternalNullifier, ExternalNullifier},
    payload::PBHPayload,
};
use world_chain_builder_test_utils::bindings::{IMulticall3, IPBHEntryPoint};

pub const PBH_ENTRYPOINT_SEPOLIA: Address = address!("7AcDc12cbCba53E1ea2206844D0A8cCb6f3B08fB");
pub const SEPOLIA_CHAIN_ID: u64 = 4801;

pub async fn generate_pbh_txs(
    identity: &Identity,
    proof: InclusionProofResponse,
    signer: PrivateKeySigner,
    quantity: u8,
    nonce: u64,
) -> eyre::Result<Vec<TxEnvelope>> {
    let root = proof.root.expect("root is required");
    let proof = proof.proof.expect("proof is required");
    let sender = signer.address();
    let signer = EthereumWallet::from(signer);

    let mut txs = vec![];

    for i in 0..quantity {
        let date = chrono::Utc::now().naive_utc().date();
        let date_marker = DateMarker::from(date);

        let external_nullifier = ExternalNullifier::with_date_marker(date_marker, 26);
        let external_nullifier_hash = EncodedExternalNullifier::from(external_nullifier).0;

        let call = IMulticall3::Call3::default();
        let calls = vec![call];
        let signal_hash: alloy_primitives::Uint<256, 4> =
            hash_to_field(&SolValue::abi_encode_packed(&(sender, calls.clone())));

        let semaphore_proof = semaphore_rs::protocol::generate_proof(
            identity,
            &proof,
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
            nonce: Some(nonce + i as u64),
            value: None,
            to: Some(TxKind::Call(PBH_ENTRYPOINT_SEPOLIA)),
            gas: Some(100000),
            max_fee_per_gas: Some(20e10 as u128),
            max_priority_fee_per_gas: Some(20e10 as u128),
            chain_id: Some(SEPOLIA_CHAIN_ID),
            input: TransactionInput {
                input: None,
                data: Some(calldata.abi_encode().into()),
            },
            from: Some(sender),
            ..Default::default()
        };

        let envelope = tx.build(&signer).await?;
        txs.push(envelope)
    }

    Ok(txs)
}

pub async fn generate_txs(
    signer: PrivateKeySigner,
    quantity: u8,
    start_nonce: u64,
) -> eyre::Result<Vec<TxEnvelope>> {
    let sender = signer.address();
    let signer = EthereumWallet::from(signer);

    let mut txs = vec![];
    for i in 0..quantity {
        let tx = TransactionRequest {
            nonce: Some(start_nonce + i as u64),
            value: None,
            to: Some(TxKind::Call(Address::random())),
            gas: Some(100000),
            max_fee_per_gas: Some(20e10 as u128),
            max_priority_fee_per_gas: Some(20e10 as u128),
            chain_id: Some(SEPOLIA_CHAIN_ID),
            input: TransactionInput {
                input: None,
                data: Some(vec![0x00].into()),
            },
            from: Some(sender),
            ..Default::default()
        };

        let envelope = <TransactionRequest as TransactionBuilder<Ethereum>>::build(tx, &signer)
            .await
            .unwrap();
        txs.push(envelope)
    }

    Ok(txs)
}
