use reth_db::table::{Compress, Decompress, Table};
use reth_primitives::B256;
use serde::{Deserialize, Serialize};

/// Using key value with a prefix would also probablly work here.
impl Table for NullifierTable {
    const NAME: &'static str = "Nullifiers";

    type Key = B256;

    type Value = EmptyValue;
}

#[derive(Debug, Clone, Default)]
pub struct NullifierTable;

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
