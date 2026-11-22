use crate::{
    Error, Result,
    config::{PbhConfig, SigningTransport},
    ed25519::HexBytes,
    transport::{Method, TRANSACTION_BODY, Transport, encode_binary_request, encode_request},
};
use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope, transaction::SignerRecoverable};
use alloy_eips::{Decodable2718, Encodable2718, eip2930::AccessList};
use alloy_primitives::{Address, Signature, TxKind};
use serde::Serialize;
use std::sync::Arc;
use tokio::time::Instant;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TransactionRequest<'a> {
    from: Address,
    to: Address,
    gas: String,
    nonce: String,
    chain_id: String,
    value: String,
    max_fee_per_gas: String,
    max_priority_fee_per_gas: String,
    input: HexBytes<'a>,
    access_list: &'a AccessList,
}

/// PBH uses only type 2, with nonce/construction/execution owned by the caller.
#[derive(Clone, Debug)]
pub struct PbhSigner {
    transport: Arc<Transport>,
    expected: Address,
}

impl PbhSigner {
    pub fn new(config: PbhConfig) -> Result<Self> {
        Ok(Self {
            transport: Arc::new(Transport::new(config.connection, "pbh")?),
            expected: config.expected_address,
        })
    }
    pub fn address(&self) -> Address {
        self.expected
    }
    pub async fn health(&self, deadline: Instant) -> Result<()> {
        self.transport.health(deadline).await
    }
    pub async fn sign(&self, transaction: &TxEip1559, deadline: Instant) -> Result<Signature> {
        let TxKind::Call(to) = transaction.to else {
            return Err(Error::Config("PBH contract creation is not supported"));
        };
        let method = Method::Transaction;
        self.transport
            .sign(
                method,
                deadline,
                |wire, id| match wire {
                    SigningTransport::JsonRpc => {
                        if transaction.input.len() > TRANSACTION_BODY / 2 {
                            return Err(Error::TooLarge("transaction calldata"));
                        }
                        encode_request(
                            method,
                            id,
                            Some(&TransactionRequest {
                                from: self.expected,
                                to,
                                gas: format!("0x{:x}", transaction.gas_limit),
                                nonce: format!("0x{:x}", transaction.nonce),
                                chain_id: format!("0x{:x}", transaction.chain_id),
                                value: format!("0x{:x}", transaction.value),
                                max_fee_per_gas: format!("0x{:x}", transaction.max_fee_per_gas),
                                max_priority_fee_per_gas: format!(
                                    "0x{:x}",
                                    transaction.max_priority_fee_per_gas
                                ),
                                input: HexBytes(&transaction.input),
                                access_list: &transaction.access_list,
                            }),
                        )
                    }
                    SigningTransport::Binary => {
                        let payload_len =
                            Address::len_bytes() + transaction.payload_len_for_signature();
                        encode_binary_request(method, id, payload_len, |body| {
                            body.extend_from_slice(self.expected.as_slice());
                            transaction.encode_for_signing(body);
                        })
                    }
                },
                |value| verify_transaction(value, transaction, self.expected),
            )
            .await
    }
}

fn verify_transaction(raw: &[u8], expected: &TxEip1559, address: Address) -> Result<Signature> {
    let mut bytes = raw;
    let envelope = TxEnvelope::decode_2718(&mut bytes)
        .map_err(|_| Error::InvalidResponse("invalid signed transaction"))?;
    if !bytes.is_empty() || envelope.encoded_2718() != raw {
        return Err(Error::InvalidResponse(
            "noncanonical or trailing transaction data",
        ));
    }
    let TxEnvelope::Eip1559(signed) = envelope else {
        return Err(Error::InvalidResponse("PBH requires type 2"));
    };
    if signed.tx() != expected {
        return Err(Error::InvalidResponse("signed transaction fields changed"));
    }
    if signed.signature().normalize_s().is_some() {
        return Err(Error::InvalidSignature);
    }
    let recovered =
        SignerRecoverable::recover_signer(&signed).map_err(|_| Error::InvalidSignature)?;
    if recovered != address {
        return Err(Error::InvalidSignature);
    }
    Ok(*signed.signature())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::decode_hex;
    use serde::Deserialize;
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Vectors {
        signer: Address,
        cases: Vec<Vector>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Vector {
        signed_transaction: String,
    }

    #[test]
    fn pbh_results_preserve_all_fields_and_expected_identity() {
        let corpus: Vectors =
            serde_json::from_str(include_str!("../../builder/testdata/pbh-signing-v1.json"))
                .expect("public corpus");
        for vector in corpus.cases {
            let bytes = decode_hex(&vector.signed_transaction, TRANSACTION_BODY).expect("bytes");
            let TxEnvelope::Eip1559(signed) =
                TxEnvelope::decode_2718(&mut bytes.as_slice()).expect("transaction")
            else {
                panic!("type 2")
            };
            let tx = signed.tx();
            assert_eq!(
                verify_transaction(&bytes, tx, corpus.signer),
                Ok(*signed.signature())
            );
            assert!(verify_transaction(&bytes, tx, Address::ZERO).is_err());
            for field in 0..9 {
                let mut changed = tx.clone();
                match field {
                    0 => changed.chain_id += 1,
                    1 => changed.nonce += 1,
                    2 => changed.gas_limit += 1,
                    3 => changed.max_fee_per_gas += 1,
                    4 => changed.max_priority_fee_per_gas += 1,
                    5 => changed.to = TxKind::Call(Address::ZERO),
                    6 => changed.value += alloy_primitives::U256::from(1),
                    7 => changed.input = vec![1].into(),
                    _ => changed.access_list.0.push(Default::default()),
                }
                assert!(verify_transaction(&bytes, &changed, corpus.signer).is_err());
            }
            let mut trailing = bytes;
            trailing.push(0);
            assert!(verify_transaction(&trailing, tx, corpus.signer).is_err());
        }
    }
}
