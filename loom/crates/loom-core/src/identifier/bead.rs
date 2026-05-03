use displaydoc::Display;
use thiserror::Error;

use super::newtype_id;

newtype_id!(BeadId);

impl BeadId {
    /// Parse a bead id from a raw string. Validates the canonical
    /// `<prefix>-<base32>(.<digits>)?` shape so that subprocess output
    /// drift (banners, warnings, empty lines) is caught at the boundary
    /// rather than producing a malformed [`BeadId`] downstream.
    ///
    /// `<prefix>` is one or more lowercase ASCII letters, `<base32>` is
    /// one or more lowercase ASCII alphanumerics, and the optional
    /// `.<digits>` suffix scopes a sub-issue under a molecule.
    pub fn parse(s: &str) -> Result<Self, ParseBeadIdError> {
        let (prefix, rest) = s
            .split_once('-')
            .ok_or_else(|| ParseBeadIdError(s.to_owned()))?;
        if prefix.is_empty() || !prefix.bytes().all(|b| b.is_ascii_lowercase()) {
            return Err(ParseBeadIdError(s.to_owned()));
        }
        let (body, suffix) = match rest.split_once('.') {
            Some((body, suffix)) => (body, Some(suffix)),
            None => (rest, None),
        };
        if body.is_empty()
            || !body
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        {
            return Err(ParseBeadIdError(s.to_owned()));
        }
        if let Some(suffix) = suffix
            && (suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(ParseBeadIdError(s.to_owned()));
        }
        Ok(Self(s.to_owned()))
    }
}

/// invalid bead id `{0}`: expected `<prefix>-<base32>(.<digits>)?`
#[derive(Debug, Display, Error, PartialEq, Eq)]
pub struct ParseBeadIdError(pub String);

#[cfg(test)]
mod tests {
    use super::{BeadId, ParseBeadIdError};
    use anyhow::Result;

    #[test]
    fn display_round_trips_with_as_str() {
        let id = BeadId::new("wx-3hhwq.2");
        assert_eq!(id.as_str(), "wx-3hhwq.2");
        assert_eq!(id.to_string(), "wx-3hhwq.2");
    }

    #[test]
    fn serde_round_trips_as_plain_string() -> Result<()> {
        let id = BeadId::new("wx-abc123");
        let json = serde_json::to_string(&id)?;
        assert_eq!(json, "\"wx-abc123\"");
        let back: BeadId = serde_json::from_str(&json)?;
        assert_eq!(back, id);
        Ok(())
    }

    #[test]
    fn parse_accepts_canonical_shapes() -> Result<()> {
        for input in ["wx-abc123", "wx-3hhwq.2", "wx-3hhwq.20", "loom-a1b2c3"] {
            let id = BeadId::parse(input)?;
            assert_eq!(id.as_str(), input);
        }
        Ok(())
    }

    #[test]
    fn parse_rejects_malformed_inputs() {
        let cases = [
            "",
            "wx",
            "wx-",
            "-abc",
            "wx-ABC",
            "wx-abc.",
            "wx-abc.x",
            "wx-abc.1.2",
            "WX-abc",
            "warning: foo\nwx-abc123",
            "wx-abc 123",
            "wx-abc-def",
        ];
        for input in cases {
            let err = BeadId::parse(input).expect_err(input);
            assert_eq!(err, ParseBeadIdError(input.to_owned()));
        }
    }
}
