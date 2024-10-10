use std::str::FromStr;

use semaphore::{hash_to_field, Field};
use thiserror::Error;

use crate::pbh::date_marker::{DateMarker, DateMarkerParsingError};

use strum::EnumString;
use strum_macros::Display;

#[derive(Display, EnumString, Debug, Clone, Copy, PartialEq, Eq)]
#[strum(serialize_all = "snake_case")]
pub enum Prefix {
    V1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalNullifier {
    pub prefix: Prefix,
    pub date_marker: DateMarker,
    pub nonce: u16,
}

impl ExternalNullifier {
    pub fn new(prefix: Prefix, date_marker: DateMarker, nonce: u16) -> Self {
        Self {
            prefix,
            date_marker,
            nonce,
        }
    }

    pub fn hash(&self) -> Field {
        hash_to_field(self.to_string().as_bytes())
    }
}

impl std::fmt::Display for ExternalNullifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}-{}", self.prefix, self.date_marker, self.nonce)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExternalNullifierParsingError {
    #[error("invalid format - expected a string of format `vv-mmyyyy-xxx...` got {actual}")]
    InvaldFormat { actual: String },

    #[error("error parsing prefix - {0}")]
    InvalidPrefix(strum::ParseError),

    #[error("error parsing date marker - {0}")]
    InvalidDateMarker(#[from] DateMarkerParsingError),

    #[error("error parsing nonce - {0}")]
    InvalidNonce(std::num::ParseIntError),

    #[error("leading zeroes in nonce `{0}`")]
    LeadingZeroes(String),
}

impl FromStr for ExternalNullifier {
    type Err = ExternalNullifierParsingError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() != 3 {
            return Err(ExternalNullifierParsingError::InvaldFormat {
                actual: s.to_string(),
            });
        }

        let prefix: Prefix = parts[0]
            .parse()
            .map_err(ExternalNullifierParsingError::InvalidPrefix)?;

        let date_marker = parts[1].parse()?;

        let nonce_str = parts[2];
        let nonce_str_trimmed = nonce_str.trim_start_matches('0');

        if nonce_str != "0" && nonce_str != nonce_str_trimmed {
            return Err(ExternalNullifierParsingError::LeadingZeroes(
                nonce_str.to_string(),
            ));
        }

        let nonce = nonce_str
            .parse()
            .map_err(ExternalNullifierParsingError::InvalidNonce)?;

        Ok(ExternalNullifier {
            prefix,
            date_marker,
            nonce,
        })
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    #[test_case("v1-012025-11")]
    #[test_case("v1-012025-19")]
    fn parse_external_nulliifer_roundtrip(s: &str) {
        let e: ExternalNullifier = s.parse().unwrap();

        assert_eq!(e.to_string(), s);
    }

    #[test_case("v2-012025-11")]
    #[test_case("v1-012025-011")]
    fn parse_external_nulliifer_invalid(s: &str) {
        s.parse::<ExternalNullifier>().unwrap_err();
    }
}
