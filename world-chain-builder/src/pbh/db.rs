use reth_db::table::{Compress, Decompress, Table};
use reth_primitives::{TxHash, B256};
use serde::{Deserialize, Serialize};

/// Table for executed nullifiers.
#[derive(Debug, Clone, Default)]
pub struct ExecutedPbhNullifierTable;

impl Table for ExecutedPbhNullifierTable {
    const NAME: &'static str = "ExecutedPbhNullifiers";

    type Key = B256;

    type Value = EmptyValue;
}

/// Table to store PBH validated transactions along with their nullifiers.
#[derive(Debug, Clone, Default)]
pub struct ValidatedPbhTransactionTable;

impl Table for ValidatedPbhTransactionTable {
    const NAME: &'static str = "ValidatedPbhTransactions";

    type Key = TxHash;

    type Value = B256;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmptyValue;

impl Decompress for EmptyValue {
    fn decompress<B: AsRef<[u8]>>(_: B) -> Result<Self, reth_db::DatabaseError> {
        Ok(Self)
    }
}

impl Compress for EmptyValue {
    type Compressed = Vec<u8>;

    fn compress_to_buf<B: reth_primitives::bytes::BufMut + AsMut<[u8]>>(self, _buf: &mut B) {}
}
