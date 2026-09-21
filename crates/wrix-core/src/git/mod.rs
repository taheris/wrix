use std::fmt;

use displaydoc::Display;
use serde::{Deserialize, Deserializer, de};
use thiserror::Error as ThisError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Branch(String);

#[derive(Debug, Display, Eq, PartialEq, ThisError)]
/// invalid Git branch name: {value}
pub struct ParseError {
    value: String,
}

impl Branch {
    pub fn parse(value: &str) -> Result<Self, ParseError> {
        if is_valid(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(ParseError {
                value: value.to_owned(),
            })
        }
    }

    pub const fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl<'de> Deserialize<'de> for Branch {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl de::Visitor<'_> for Visitor {
            type Value = Branch;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a Git branch string")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Branch, E> {
                Branch::parse(value).map_err(E::custom)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

impl Default for Branch {
    fn default() -> Self {
        Self(String::from("beads"))
    }
}

impl fmt::Display for Branch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn is_valid(value: &str) -> bool {
    !value.is_empty()
        && value != "@"
        && !value.starts_with('-')
        && !value.ends_with('.')
        && !value.contains("..")
        && !value.contains("@{")
        && !value.bytes().any(is_forbidden_byte)
        && value.split('/').all(|component| {
            !component.is_empty()
                && !component.starts_with('.')
                && !component.as_bytes().ends_with(b".lock")
        })
}

const fn is_forbidden_byte(byte: u8) -> bool {
    byte <= b' ' || byte == 0x7f || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
}

#[cfg(test)]
mod test {
    use super::Branch;

    #[test]
    fn branch_parser_accepts_hierarchical_names() {
        let branch = Branch::parse("team/beads-sync").unwrap();

        assert_eq!(branch.as_str(), "team/beads-sync");
    }

    #[test]
    fn branch_parser_rejects_invalid_git_ref_names() {
        for value in [
            "",
            "@",
            "-beads",
            "/",
            "/outside",
            "../outside",
            "team/",
            ".beads",
            "team/.beads",
            "team//beads",
            "team/../beads",
            "team/beads.lock",
            "team/beads.",
            "team/beads ",
            "team/beads~1",
            "team/@{beads",
        ] {
            assert!(Branch::parse(value).is_err(), "accepted {value:?}");
        }
    }
}
