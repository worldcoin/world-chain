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

    pub fn to_word(&self) -> U256 {
        let year = U256::from(self.year);
        let month = U256::from(self.month);
        let nonce = U256::from(self.nonce);
        let version = U256::from(self.version as u8);

        let year_shifted = year << 24;
        let month_shifted = month << 16;
        let nonce_shifted = nonce << 8;

        year_shifted | month_shifted | nonce_shifted | version
    }

    pub fn from_word(word: U256) -> Self {
        let year: u16 = (word >> U256::from(24)).to();
        let month: u8 = ((word >> U256::from(16)) & U256::from(0xFF)).to();
        let nonce: u8 = ((word >> U256::from(8)) & U256::from(0xFF)).to();
        Self {
            version: Prefix::V1,
            year,
            month,
            nonce,
        }
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
        Ok(Self::from_word(word))
    }
}

impl Decodable for ExternalNullifier {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let word = U256::decode(buf)?;
        Ok(Self::from_word(word))
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
