use chrono::{Datelike, NaiveDate};
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateMarker {
    pub year: i32,
    pub month: u32,
}

impl DateMarker {
    pub fn new(year: i32, month: u32) -> Self {
        Self { year, month }
    }
}

impl<T> From<T> for DateMarker
where
    T: Datelike,
{
    fn from(value: T) -> Self {
        Self {
            year: value.year(),
            month: value.month(),
        }
    }
}

impl From<DateMarker> for NaiveDate {
    fn from(value: DateMarker) -> Self {
        NaiveDate::from_ymd_opt(value.year, value.month, 1).unwrap()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DateMarkerParsingError {
    #[error("invalid length - expected 6 characters got {actual}")]
    InvaldLength { actual: usize },
    #[error("error parsing month - {0}")]
    InvalidMonth(std::num::ParseIntError),
    #[error("month out of range - expected 01-12 got {month}")]
    MonthOutOfRange { month: u32 },
    #[error("error parsing year - {0}")]
    InvalidYear(std::num::ParseIntError),
}

impl FromStr for DateMarker {
    type Err = DateMarkerParsingError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 6 {
            return Err(DateMarkerParsingError::InvaldLength { actual: s.len() });
        }

        let month = &s[..2];
        let year = &s[2..];

        let month = month
            .parse()
            .map_err(DateMarkerParsingError::InvalidMonth)?;
        let year = year.parse().map_err(DateMarkerParsingError::InvalidYear)?;

        if !(1..=12).contains(&month) {
            return Err(DateMarkerParsingError::MonthOutOfRange { month });
        }

        Ok(DateMarker { year, month })
    }
}

impl std::fmt::Display for DateMarker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02}{:04}", self.month, self.year)
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    #[test_case("012024")]
    #[test_case("022024")]
    #[test_case("022025")]
    fn parse_month_marker_roundtrip(s: &str) {
        let m: DateMarker = s.parse().unwrap();

        assert_eq!(m.to_string(), s);
    }

    #[test_case("132024" ; "invalid month")]
    #[test_case("12024" ; "too short")]
    #[test_case("003024" ; "zero month")]
    #[test_case("" ; "empty")]
    #[test_case("23012024" ; "too long")]
    fn parse_month_marker_invalid(s: &str) {
        s.parse::<DateMarker>().unwrap_err();
    }
}
