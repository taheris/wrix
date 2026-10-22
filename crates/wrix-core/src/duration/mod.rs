//! Human duration parsing with exact destination units.

use displaydoc::Display;
use thiserror::Error;

#[derive(Debug, Display, Error)]
pub enum ParseError {
    /// invalid duration: {0}
    Syntax(#[from] humantime::DurationError),
    /// duration is not exactly representable in {0}
    Precision(&'static str),
    /// duration overflows {0}
    Overflow(&'static str),
    /// retention must be strictly positive
    ZeroRetention,
}

/// Parse without losing subsecond precision; callers own their finite deadline checks.
pub fn parse(input: &str) -> Result<std::time::Duration, ParseError> {
    Ok(humantime::parse_duration(input)?)
}

pub fn seconds(input: &str) -> Result<u64, ParseError> {
    let duration = parse(input)?;
    if duration.subsec_nanos() != 0 {
        return Err(ParseError::Precision("seconds"));
    }
    Ok(duration.as_secs())
}

pub fn milliseconds(input: &str) -> Result<u64, ParseError> {
    let duration = parse(input)?;
    if duration.subsec_nanos() % 1_000_000 != 0 {
        return Err(ParseError::Precision("milliseconds"));
    }
    u64::try_from(duration.as_millis()).map_err(|_source| ParseError::Overflow("milliseconds"))
}

/// Native cache retention uses positive signed 32-bit seconds.
pub fn retention_seconds(input: &str) -> Result<i32, ParseError> {
    let seconds = seconds(input)?;
    if seconds == 0 {
        return Err(ParseError::ZeroRetention);
    }
    i32::try_from(seconds).map_err(|_source| ParseError::Overflow("native retention seconds"))
}
