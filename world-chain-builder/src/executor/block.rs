use alloy_rlp::{RlpDecodable, RlpEncodable};
use derive_more::{Deref, DerefMut};
use reth_primitives::{Block, BlockWithSenders, Header, Requests, TransactionSigned, Withdrawals};
use revm_primitives::Address;
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, Deref, RlpEncodable, RlpDecodable,
)]
#[rlp(trailing)]
pub struct WorldCoinBlock {
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

impl From<WorldCoinBlock> for Block {
    fn from(block: WorldCoinBlock) -> Self {
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
pub struct WorldCoinBlockWithSenders {
    /// Block
    #[deref]
    #[deref_mut]
    pub block: WorldCoinBlock,
    /// List of senders that match the transactions in the block
    pub senders: Vec<Address>,
}

impl From<WorldCoinBlockWithSenders> for BlockWithSenders {
    fn from(block: WorldCoinBlockWithSenders) -> Self {
        Self {
            block: block.block.into(),
            senders: block.senders,
        }
    }
}
