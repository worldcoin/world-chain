use alloy_rlp::{RlpDecodable, RlpEncodable};
use derive_more::{Deref, DerefMut};
use reth_primitives::{Block, BlockWithSenders, Header, Requests, TransactionSigned, Withdrawals};
use reth_provider::BlockExecutionInput;
use revm_primitives::Address;
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, Deref, RlpEncodable, RlpDecodable,
)]
#[rlp(trailing)]
pub struct WcBlock {
    /// Block header.
    #[deref]
    pub header: Header,
    /// Transactions in this block.
    pub body: Vec<TransactionSigned>,
    /// Ommers/uncles header.
    pub ommers: Vec<Header>,
    /// Block withdrawals.
    pub withdrawals: Option<Withdrawals>,
    /// Block requests.
    pub requests: Option<Requests>,
}

impl From<WcBlock> for Block {
    fn from(block: WcBlock) -> Self {
        Self {
            header: block.header,
            body: block.body,
            ommers: block.ommers,
            withdrawals: block.withdrawals,
            requests: block.requests,
        }
    }
}

/// Sealed block with senders recovered from transactions.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deref, DerefMut)]
pub struct WcBlockWithSenders {
    /// Block
    #[deref]
    #[deref_mut]
    pub block: WcBlock,
    /// List of senders that match the transactions in the block
    pub senders: Vec<Address>,
}

impl From<WcBlockWithSenders> for BlockWithSenders {
    fn from(block: WcBlockWithSenders) -> Self {
        Self {
            block: block.block.into(),
            senders: block.senders,
        }
    }
}

// impl<'a> From<BlockExecutionInput<'a, WcBlockWithSenders>>
//     for BlockExecutionInput<'a, WcBlockWithSenders>
// {
//     fn from(value: BlockExecutionInput<'a, WcBlockWithSenders>) -> Self {
//         todo!()
//     }
// }
