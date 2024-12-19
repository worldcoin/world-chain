use std::str::FromStr;

use alloy_primitives::{ruint, U256};
use alloy_rlp::{Decodable, Encodable};
use bon::Builder;
use strum::{Display, EnumString};
use thiserror::Error;

use crate::date_marker::DateMarker;

#[derive(Display, EnumString, Debug, Clone, Copy, PartialEq, Eq)]
#[strum(serialize_all = "snake_case")]
#[repr(u8)]
pub enum Prefix {
    V1 = 1,
}

#[derive(Builder, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalNullifier {
    #[builder(default = Prefix::V1)]
    pub version: Prefix,
    #[builder(into)]
    pub year: u16,
    #[builder(into)]
    pub month: u8,
    #[builder(into, default = 0)]
    pub nonce: u8,
}

impl ExternalNullifier {
    pub fn with_date_marker(marker: DateMarker, nonce: u8) -> Self {
        Self::v1(marker.month as u8, marker.year as u16, nonce)
    }

    pub fn v1(month: u8, year: u16, nonce: u8) -> Self {
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

    pub fn be_bytes(&self) -> [u8; 32] {
        let year_bytes = self.year.to_be_bytes();

        let mut bytes = [0; 32];

        bytes[4] = year_bytes[1];
        bytes[3] = year_bytes[0];
        bytes[2] = self.month;
        bytes[1] = self.nonce;
        bytes[0] = self.version as u8;

        bytes
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self::try_from_bytes(bytes).expect("Invalid version")
    }

    pub fn try_from_bytes(bytes: [u8; 32]) -> Result<Self, ExternalNullifierError> {
        let version = if bytes[0] == Prefix::V1 as u8 {
            Prefix::V1
        } else {
            return Err(ExternalNullifierError::InvalidVersion);
        };

        let mut year_bytes = [0; 2];
        year_bytes[1] = bytes[4];
        year_bytes[0] = bytes[3];
        let year = u16::from_be_bytes(year_bytes);

        let month = bytes[2];

        if month > 12 {
            return Err(ExternalNullifierError::InvalidMonth(month));
        }

        let nonce = bytes[1];

        Ok(Self {
            version,
            year,
            month,
            nonce,
        })
    }

    pub fn to_word(&self) -> U256 {
        let bytes = self.be_bytes();

        U256::from_be_bytes(bytes)
    }

    pub fn from_word(word: U256) -> Self {
        Self::try_from_word(word).expect("Invalid version")
    }

    pub fn try_from_word(word: U256) -> Result<Self, ExternalNullifierError> {
        let bytes = word.to_be_bytes();

        Self::try_from_bytes(bytes)
    }
}

impl std::fmt::Display for ExternalNullifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let w = self.to_word();
        std::fmt::Display::fmt(&w, f)
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
}

impl FromStr for ExternalNullifier {
    type Err = ExternalNullifierError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let word: U256 = s.parse()?;

        Self::try_from_word(word)
    }
}

impl Decodable for ExternalNullifier {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let word = U256::decode(buf)?;

        // TODO: How to retrieve this error value? Maybe just log?
        Self::try_from_word(word)
            .map_err(|_err| alloy_rlp::Error::Custom("Invalid external nullifier version"))
    }
}

impl Encodable for ExternalNullifier {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        let word = self.to_word();

        word.encode(out);
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
    }
}
