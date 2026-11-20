//! Verify receipt and transaction commitments for one authenticated Flashblock view.

use alloy_consensus::Header;
use alloy_eips::eip2718::Decodable2718;
use alloy_primitives::{B256, Bytes, Log, keccak256};
use alloy_trie::{
    HashBuilder, Nibbles,
    proof::{ProofRetainer, verify_proof},
    root::{adjust_index_for_rlp, ordered_trie_root_encoded},
};
use ed25519_dalek::VerifyingKey;
use op_alloy_consensus::OpReceiptEnvelope;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::evidence::{FlashblockEvidence, FlashblockEvidenceError, FlashblockEvidenceId};

pub const CAPABILITY: &str = "worldchain_flashblock_receipt_proofs_v1";
pub const MAX_VIEW_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_TRANSACTIONS: usize = 16_384;
pub const MAX_BUNDLE_BYTES: usize = 2 * MAX_VIEW_BYTES + 32 * 1024;
pub const MAX_BUNDLE_JSON_BYTES: usize = 2 * MAX_BUNDLE_BYTES + 128 * 1024;
const MAX_HEADER_BYTES: usize = 1024;
const MAX_PROOF_NODES: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlashblockReceiptProof {
    pub version: u32,
    pub id: FlashblockEvidenceId,
    /// Canonical RLP of the header identified by the signed block hash.
    pub header: Bytes,
    pub transaction_index: u64,
    /// Canonical EIP-2718 bytes committed to the transaction and receipt tries.
    pub transaction: Bytes,
    pub receipt: Bytes,
    pub transaction_proof: Vec<Bytes>,
    pub receipt_proof: Vec<Bytes>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlashblockReceipts {
    pub version: u32,
    pub id: FlashblockEvidenceId,
    pub header: Bytes,
    /// Canonical OP receipt bytes in transaction index order.
    pub receipts: Vec<Bytes>,
}

#[derive(Debug, Error)]
pub enum ReceiptProofError {
    #[error("unsupported receipt proof version or mismatched view")]
    View,
    #[error("receipt proof exceeds size or item limits")]
    Limit,
    #[error(transparent)]
    Evidence(#[from] FlashblockEvidenceError),
    #[error("header does not match the signed block hash or receipts root")]
    Header,
    #[error("transaction does not match the requested hash")]
    Transaction,
    #[error("invalid transaction trie proof")]
    TransactionProof,
    #[error("invalid receipt trie proof")]
    ReceiptProof,
    #[error("ordered receipts do not match the authenticated root")]
    ReceiptsRoot,
    #[error("invalid canonical OP receipt")]
    ReceiptEncoding,
    #[error("log does not match the requested receipt-local ordinal and contents")]
    Log,
}

impl FlashblockReceiptProof {
    /// Authenticate the selected transaction and its receipt at the same trie index.
    pub fn verify_receipt(
        &self,
        evidence: &FlashblockEvidence,
        chain_id: u64,
        authorizer: VerifyingKey,
        expected: FlashblockEvidenceId,
        transaction_hash: B256,
    ) -> Result<OpReceiptEnvelope, ReceiptProofError> {
        self.check_limits()?;
        let header = authenticated_header(
            self.version,
            self.id,
            &self.header,
            evidence,
            chain_id,
            authorizer,
            expected,
        )?;
        if keccak256(&self.transaction) != transaction_hash {
            return Err(ReceiptProofError::Transaction);
        }
        let key = Nibbles::unpack(alloy_rlp::encode_fixed_size(&self.transaction_index));
        verify_proof(
            header.transactions_root,
            key,
            Some(self.transaction.to_vec()),
            &self.transaction_proof,
        )
        .map_err(|_| ReceiptProofError::TransactionProof)?;
        verify_proof(
            header.receipts_root,
            key,
            Some(self.receipt.to_vec()),
            &self.receipt_proof,
        )
        .map_err(|_| ReceiptProofError::ReceiptProof)?;
        decode_receipt(&self.receipt)
    }

    pub fn check_limits(&self) -> Result<(), ReceiptProofError> {
        let bytes = self
            .header
            .len()
            .saturating_add(self.transaction.len())
            .saturating_add(self.receipt.len());
        if self.transaction_index >= MAX_TRANSACTIONS as u64
            || self.header.len() > MAX_HEADER_BYTES
            || self.transaction_proof.len() > MAX_PROOF_NODES
            || self.receipt_proof.len() > MAX_PROOF_NODES
            || !fits_budget(
                bytes,
                self.transaction_proof.iter().chain(&self.receipt_proof),
                MAX_BUNDLE_BYTES,
            )
        {
            return Err(ReceiptProofError::Limit);
        }
        Ok(())
    }
}

/// Compare a log already obtained from an authenticated receipt by its receipt-local ordinal.
pub fn verify_log(
    receipt: &OpReceiptEnvelope,
    ordinal: usize,
    expected: &Log,
) -> Result<(), ReceiptProofError> {
    if receipt.logs().get(ordinal) != Some(expected) {
        return Err(ReceiptProofError::Log);
    }
    Ok(())
}

impl FlashblockReceipts {
    /// Verify completeness and recover all receipts in transaction order for this view.
    pub fn verify(
        &self,
        evidence: &FlashblockEvidence,
        chain_id: u64,
        authorizer: VerifyingKey,
        expected: FlashblockEvidenceId,
    ) -> Result<Vec<OpReceiptEnvelope>, ReceiptProofError> {
        if self.receipts.len() > MAX_TRANSACTIONS
            || self.header.len() > MAX_HEADER_BYTES
            || !fits_budget(self.header.len(), self.receipts.iter(), MAX_VIEW_BYTES)
        {
            return Err(ReceiptProofError::Limit);
        }
        let header = authenticated_header(
            self.version,
            self.id,
            &self.header,
            evidence,
            chain_id,
            authorizer,
            expected,
        )?;
        if ordered_trie_root_encoded(&self.receipts) != header.receipts_root {
            return Err(ReceiptProofError::ReceiptsRoot);
        }
        self.receipts
            .iter()
            .map(|receipt| decode_receipt(receipt))
            .collect()
    }
}

/// Build an inclusion proof over the canonical encoded values of an ordered trie.
pub fn ordered_proof(values: &[Bytes], index: usize) -> Option<(B256, Vec<Bytes>)> {
    if index >= values.len() || values.len() > MAX_TRANSACTIONS {
        return None;
    }
    let key = Nibbles::unpack(alloy_rlp::encode_fixed_size(&index));
    let mut builder = HashBuilder::default().with_proof_retainer(ProofRetainer::new(vec![key]));
    for i in 0..values.len() {
        let index = adjust_index_for_rlp(i, values.len());
        let key = Nibbles::unpack(alloy_rlp::encode_fixed_size(&index));
        builder.add_leaf(key, &values[index]);
    }
    let root = builder.root();
    let nodes = builder
        .take_proof_nodes()
        .into_nodes_sorted()
        .into_iter()
        // The verifier follows embedded children directly from their parent.
        .filter_map(|(path, bytes)| (path.is_empty() || bytes.len() >= 32).then_some(bytes))
        .collect();
    Some((root, nodes))
}

fn fits_budget<'a>(initial: usize, bytes: impl Iterator<Item = &'a Bytes>, limit: usize) -> bool {
    bytes.fold(initial, |total, item| total.saturating_add(item.len())) <= limit
}

fn decode_receipt(bytes: &[u8]) -> Result<OpReceiptEnvelope, ReceiptProofError> {
    OpReceiptEnvelope::decode_2718_exact(bytes).map_err(|_| ReceiptProofError::ReceiptEncoding)
}

fn authenticated_header(
    version: u32,
    id: FlashblockEvidenceId,
    rlp: &Bytes,
    evidence: &FlashblockEvidence,
    chain_id: u64,
    authorizer: VerifyingKey,
    expected: FlashblockEvidenceId,
) -> Result<Header, ReceiptProofError> {
    if version != 1 || id != expected {
        return Err(ReceiptProofError::View);
    }
    let payload = evidence.verify(chain_id, authorizer, expected)?;
    if keccak256(rlp) != expected.block_hash {
        return Err(ReceiptProofError::Header);
    }
    let header: Header = alloy_rlp::decode_exact(rlp).map_err(|_| ReceiptProofError::Header)?;
    if header.receipts_root != payload.diff.receipts_root {
        return Err(ReceiptProofError::Header);
    }
    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        p2p::{Authorization, Authorized, AuthorizedMsg, FlashblocksP2PMsg},
        primitives::{ExecutionPayloadFlashblockDeltaV1, FlashblocksPayloadV1},
    };
    use alloy_consensus::{Receipt, ReceiptWithBloom};
    use alloy_eips::eip2718::Encodable2718;
    use alloy_primitives::{Address, LogData};
    use ed25519_dalek::SigningKey;
    use op_alloy_consensus::OpDepositReceipt;

    fn fixture() -> (
        FlashblockEvidence,
        VerifyingKey,
        FlashblockReceiptProof,
        FlashblockReceipts,
        Log,
    ) {
        let log = Log {
            address: Address::with_last_byte(1),
            data: LogData::new(vec![B256::with_last_byte(2)], Bytes::from_static(&[3]))
                .expect("fixture log"),
        };
        let receipts: Vec<Bytes> = [
            OpReceiptEnvelope::Deposit(ReceiptWithBloom::new(
                OpDepositReceipt {
                    inner: Receipt {
                        status: true.into(),
                        cumulative_gas_used: 21_000,
                        logs: vec![],
                    },
                    deposit_nonce: Some(7),
                    deposit_receipt_version: Some(1),
                },
                Default::default(),
            )),
            OpReceiptEnvelope::Eip1559(ReceiptWithBloom::new(
                Receipt {
                    status: true.into(),
                    cumulative_gas_used: 42_000,
                    logs: vec![log.clone(), log.clone()],
                },
                Default::default(),
            )),
        ]
        .into_iter()
        .map(|receipt| receipt.encoded_2718().into())
        .collect();
        let txs = [
            Bytes::from_static(&[0x7e; 64]),
            Bytes::from_static(b"transaction"),
        ];
        let (tx_root, transaction_proof) = ordered_proof(&txs, 1).expect("transaction trie");
        let (receipts_root, receipt_proof) = ordered_proof(&receipts, 1).expect("receipt trie");
        let header = Bytes::from(alloy_rlp::encode(Header {
            transactions_root: tx_root,
            receipts_root,
            ..Default::default()
        }));
        let payload = FlashblocksPayloadV1 {
            diff: ExecutionPayloadFlashblockDeltaV1 {
                block_hash: keccak256(&header),
                receipts_root,
                ..Default::default()
            },
            ..Default::default()
        };
        let id = FlashblockEvidenceId::from(&payload);
        let authorizer = SigningKey::from_bytes(&[1; 32]);
        let actor = SigningKey::from_bytes(&[2; 32]);
        let auth = Authorization::new(payload.payload_id, 1, &authorizer, actor.verifying_key());
        let wire = FlashblocksP2PMsg::Authorized(Authorized::new(
            &actor,
            auth,
            AuthorizedMsg::FlashblocksPayloadV1(payload),
        ))
        .encode()
        .freeze()
        .into();
        (
            FlashblockEvidence {
                version: 1,
                chain_id: 480,
                id,
                wire,
            },
            authorizer.verifying_key(),
            FlashblockReceiptProof {
                version: 1,
                id,
                header: header.clone(),
                transaction_index: 1,
                transaction: txs[1].clone(),
                receipt: receipts[1].clone(),
                transaction_proof,
                receipt_proof,
            },
            FlashblockReceipts {
                version: 1,
                id,
                header,
                receipts,
            },
            log,
        )
    }

    #[test]
    fn authenticates_exact_receipts_and_log_ordinals_and_rejects_incomplete_sets() {
        let (evidence, authorizer, proof, mut all, log) = fixture();
        let tx_hash = keccak256(&proof.transaction);
        let receipt = proof
            .verify_receipt(&evidence, 480, authorizer, evidence.id, tx_hash)
            .expect("matching proofs");
        verify_log(&receipt, 0, &log).expect("first log");
        verify_log(&receipt, 1, &log).expect("identical second log");
        assert!(verify_log(&receipt, 2, &log).is_err());
        let mut wrong = log.clone();
        wrong.address = Address::ZERO;
        assert!(verify_log(&receipt, 0, &wrong).is_err());
        let verified = all
            .verify(&evidence, 480, authorizer, evidence.id)
            .expect("complete");
        assert!(matches!(&verified[0], OpReceiptEnvelope::Deposit(receipt)
            if receipt.receipt.deposit_nonce == Some(7) && receipt.receipt.deposit_receipt_version == Some(1)));
        assert_eq!(verified[1], receipt);
        all.receipts.remove(0);
        assert!(matches!(
            all.verify(&evidence, 480, authorizer, evidence.id),
            Err(ReceiptProofError::ReceiptsRoot)
        ));
    }

    #[test]
    fn rejects_unbound_commitments() {
        let (evidence, authorizer, proof, _, _) = fixture();
        let tx_hash = keccak256(&proof.transaction);
        let verify = |proof: &FlashblockReceiptProof| {
            proof.verify_receipt(&evidence, 480, authorizer, evidence.id, tx_hash)
        };
        let mut changed = proof.clone();
        changed.transaction_index = 0;
        assert!(matches!(
            verify(&changed),
            Err(ReceiptProofError::TransactionProof)
        ));
        changed = proof.clone();
        changed.receipt_proof.pop();
        assert!(matches!(
            verify(&changed),
            Err(ReceiptProofError::ReceiptProof)
        ));
        changed = proof.clone();
        changed.receipt = Bytes::from_static(b"wrong receipt");
        assert!(matches!(
            verify(&changed),
            Err(ReceiptProofError::ReceiptProof)
        ));
        changed = proof.clone();
        changed.header = Bytes::from_static(b"wrong header");
        assert!(matches!(verify(&changed), Err(ReceiptProofError::Header)));
        changed = proof.clone();
        changed.id.index += 1;
        assert!(matches!(verify(&changed), Err(ReceiptProofError::View)));
        assert!(matches!(
            proof.verify_receipt(&evidence, 480, authorizer, evidence.id, B256::ZERO),
            Err(ReceiptProofError::Transaction)
        ));
    }

    #[test]
    fn ordered_proof_matches_alloy_at_rlp_index_boundaries() {
        let values: Vec<_> = (0u16..130)
            .map(|i| {
                if i == 127 {
                    return Bytes::copy_from_slice(&i.to_be_bytes());
                }
                Bytes::from([i.to_be_bytes().as_slice(); 20].concat())
            })
            .collect();
        for index in [0usize, 1, 127, 128, 129] {
            let (root, nodes) = ordered_proof(&values, index).expect("indexed item");
            let key = Nibbles::unpack(alloy_rlp::encode_fixed_size(&index));
            assert_eq!(root, ordered_trie_root_encoded(&values));
            assert!(nodes.len() <= MAX_PROOF_NODES);
            verify_proof(root, key, Some(values[index].to_vec()), &nodes)
                .expect("proof for exact RLP key");
        }
        assert!(ordered_proof(&values, values.len()).is_none());
    }
}
