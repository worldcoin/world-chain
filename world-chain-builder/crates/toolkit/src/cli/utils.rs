use bytes::BytesMut;
use serde::{Deserialize, Serialize};

pub fn bytes_parse_hex(s: &str) -> eyre::Result<BytesMut> {
    Ok(BytesMut::from(
        &hex::decode(s.trim_start_matches("0x"))?[..],
    ))
}

pub fn parse_from_json<'a, T>(s: &'a str) -> eyre::Result<T>
where
    T: Deserialize<'a>,
{
    Ok(serde_json::from_str(s)?)
}
