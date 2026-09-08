use alloy_consensus::{SignableTransaction, TxEip1559, TxEip2930, TxEip7702, TxLegacy};
use alloy_eips::Encodable2718;
use alloy_primitives::{Bytes, Signature};
use op_alloy_consensus::{OpTxEnvelope, TxDeposit, build_post_exec_tx};
use world_chain_chainspec::WorldChainSpec;
use world_chain_primitives::{
    flashblocks::{Flashblock, recovered_block_from_flashblocks},
    primitives::{ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1, FlashblocksPayloadV1},
};
use world_chain_validator::validator::decode_transactions_with_indices;

fn transactions() -> Vec<Bytes> {
    let signature = Signature::test_signature();
    let transactions: Vec<OpTxEnvelope> = vec![
        TxLegacy::default().into_signed(signature).into(),
        TxEip2930::default().into_signed(signature).into(),
        TxEip1559::default().into_signed(signature).into(),
        TxEip7702::default().into_signed(signature).into(),
        TxDeposit::default().into(),
        build_post_exec_tx(1, vec![]).into(),
    ];
    transactions
        .into_iter()
        .map(|tx| tx.encoded_2718().into())
        .collect()
}

fn flashblock(transactions: Vec<Bytes>) -> Flashblock {
    Flashblock {
        flashblock: FlashblocksPayloadV1 {
            base: Some(ExecutionPayloadBaseV1::default()),
            diff: ExecutionPayloadFlashblockDeltaV1 {
                transactions,
                ..Default::default()
            },
            ..Default::default()
        },
    }
}

#[test]
fn accepts_canonical_transactions() {
    let transactions = transactions();
    let decoded = decode_transactions_with_indices(&transactions, 7).expect("valid transactions");
    assert_eq!(decoded.len(), transactions.len());
    for (i, (index, tx)) in decoded.iter().enumerate() {
        assert_eq!(*index, 7 + i as u64);
        assert_eq!(tx.encoded_2718(), transactions[i]);
    }
    let block = recovered_block_from_flashblocks(
        WorldChainSpec::mainnet(),
        flashblock(transactions.clone()),
    )
    .expect("valid flashblock");
    assert_eq!(block.body().transactions.len(), transactions.len());
    for (tx, encoded) in block.body().transactions.iter().zip(transactions) {
        assert_eq!(tx.encoded_2718(), encoded);
    }
}

#[test]
fn rejects_non_canonical_transactions() {
    let transactions = transactions();
    let mut malformed = vec![Bytes::new(), Bytes::from_static(&[0xff])];
    let mut tagged_legacy = vec![0x00];
    tagged_legacy.extend_from_slice(&transactions[0]);
    malformed.push(tagged_legacy.into());
    for (i, tx) in transactions.iter().enumerate() {
        let mut trailing = tx.to_vec();
        trailing.push(0x00);
        malformed.push(trailing.into());
        if i > 0 {
            malformed.push(tx.slice(1..));
        }
    }
    for tx in malformed {
        let input = vec![transactions[0].clone(), tx.clone()];
        assert!(
            decode_transactions_with_indices(&input, 7).is_err(),
            "accepted {tx}"
        );
        assert!(
            recovered_block_from_flashblocks(WorldChainSpec::mainnet(), flashblock(input)).is_err(),
            "accepted {tx}"
        );
    }
}
