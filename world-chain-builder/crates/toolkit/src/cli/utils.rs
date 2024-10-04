use bytes::{Bytes, BytesMut};
use serde::de::DeserializeOwned;

pub fn bytes_mut_parse_hex(s: &str) -> eyre::Result<BytesMut> {
    Ok(BytesMut::from(
        &hex::decode(s.trim_start_matches("0x"))?[..],
    ))
}

pub fn bytes_parse_hex(s: &str) -> eyre::Result<Bytes> {
    Ok(Bytes::from(
        hex::decode(s.trim_start_matches("0x"))?,
    ))
}

pub fn parse_from_json<'a, T>(s: &'a str) -> eyre::Result<T>
where
    T: DeserializeOwned,
{
    Ok(serde_json::from_str(s)?)
}
