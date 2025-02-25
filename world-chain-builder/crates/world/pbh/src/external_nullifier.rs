use std::str::FromStr;

use alloy_primitives::{ruint, U256};
use alloy_rlp::{Decodable, Encodable};
use bon::Builder;
use strum::{Display, EnumString};
use thiserror::Error;

use crate::date_marker::DateMarker;

#[derive(Display, Default, EnumString, Debug, Clone, Copy, PartialEq, Eq)]
#[strum(serialize_all = "snake_case")]
#[repr(u8)]
pub enum Prefix {
    #[default]
    V1 = 1,
}

#[derive(Builder, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ExternalNullifier {
    #[builder(default = Prefix::V1)]
    pub version: Prefix,
    #[builder(into)]
    pub year: u16,
    #[builder(into)]
    pub month: u8,
    #[builder(default = 0)]
    pub nonce: u16,
}

/// The encoding format is as follows:
///      - Bits:48-263: Empty
///      - Bits 40-47: Year
///      - Bits 24-39: Month
///      - Bits 8-23: Nonce
///      - Bits 0-7: Version
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodedExternalNullifier(pub U256);

impl ExternalNullifier {
    pub fn with_date_marker(marker: DateMarker, nonce: u16) -> Self {
        Self::v1(marker.month as u8, marker.year as u16, nonce)
    }

    pub fn v1(month: u8, year: u16, nonce: u16) -> Self {
        Self {
            version: Prefix::V1,
            year,
            month,
            nonce,
        }
    }

    pub fn date_marker(&self) -> DateMarker {
        DateMarker::new(self.year as i32, self.month as u32)
    }
}

impl From<ExternalNullifier> for EncodedExternalNullifier {
    fn from(e: ExternalNullifier) -> Self {
        EncodedExternalNullifier(U256::from(
            (e.year as u64) << 32
                | (e.month as u64) << 24
                | (e.nonce as u64) << 8
                | e.version as u64,
        ))
    }
}

impl TryFrom<EncodedExternalNullifier> for ExternalNullifier {
    type Error = alloy_rlp::Error;

    fn try_from(value: EncodedExternalNullifier) -> Result<Self, Self::Error> {
        if value.0 > U256::from(1) << 48 {
            return Err(alloy_rlp::Error::Custom("invalid external nullifier"));
        }

        let word: u64 = value.0.to();
        let year = (word >> 32) as u16;
        let month = ((word >> 24) & 0xFF) as u8;
        let nonce = ((word >> 8) & 0xFFFF) as u16;
        let version = (word & 0xFF) as u8;

        if version != Prefix::V1 as u8 {
            return Err(alloy_rlp::Error::Custom(
                "invalid external nullifier version",
            ));
        }

        Ok(Self {
            version: Prefix::V1,
            year,
            month,
            nonce,
        })
    }
}

impl std::fmt::Display for ExternalNullifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let word = EncodedExternalNullifier::from(*self).0;
        write!(f, "{}", word)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExternalNullifierError {
    #[error("invalid format: {0}")]
    InvalidFormat(#[from] ruint::ParseError),

    #[error("{0} is not a valid month number")]
    InvalidMonth(u8),

    #[error("error parsing external nullifier version")]
    InvalidVersion,

    #[error("error parsing external nullifier")]
    InvalidExternalNullifier,

    #[error(transparent)]
    RlpError(#[from] alloy_rlp::Error),
}

impl FromStr for ExternalNullifier {
    type Err = ExternalNullifierError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let word: U256 = s.parse()?;
        Ok(Self::try_from(EncodedExternalNullifier(word))?)
    }
}

impl Decodable for ExternalNullifier {
    fn decode(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        let word = U256::decode(buf)?;
        Ok(Self::try_from(EncodedExternalNullifier(word))?)
    }
}

impl Encodable for ExternalNullifier {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        EncodedExternalNullifier::from(*self).encode(out);
    }
}

impl Encodable for EncodedExternalNullifier {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        self.0.encode(out);
    }
}

impl Decodable for EncodedExternalNullifier {
    fn decode(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        let word = U256::decode(buf)?;
        Ok(Self(word))
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    #[test_case(ExternalNullifier::v1(1, 2025, 11))]
    #[test_case(ExternalNullifier::v1(12, 3078, 19))]
    fn parse_external_nulliifer_roundtrip(e: ExternalNullifier) {
        let s = e.to_string();
        let actual: ExternalNullifier = s.parse().unwrap();

        assert_eq!(actual, e);
    }

    #[test_case(ExternalNullifier::v1(1, 2025, 11))]
    #[test_case(ExternalNullifier::v1(12, 3078, 19))]
    fn rlp_roundtrip(e: ExternalNullifier) {
        let mut buffer = vec![];
        e.encode(&mut buffer);
        let decoded = ExternalNullifier::decode(&mut buffer.as_slice()).unwrap();
        assert_eq!(e, decoded);
        let encoded = EncodedExternalNullifier::from(e);
        let mut buffer = vec![];
        encoded.encode(&mut buffer);
        let decoded = EncodedExternalNullifier::decode(&mut buffer.as_slice()).unwrap();
        assert_eq!(encoded, decoded);
    }
}
