//! The image consumers use literal paths and one fixed ASCII store-hash class.

use std::io;

/// Reject Python constructs with different Rust semantics rather than mis-match.
pub fn validate(pattern: &str) -> io::Result<()> {
    let mut remaining = pattern.as_bytes();
    while !remaining.is_empty() {
        if let Some(rest) = remaining.strip_prefix(b"[a-z0-9]{32}") {
            remaining = rest;
        } else if let Some(rest) = remaining.strip_prefix(b"\\.") {
            remaining = rest;
        } else if remaining[0].is_ascii_alphanumeric() || b"/_-".contains(&remaining[0]) {
            remaining = &remaining[1..];
        } else {
            return Err(io::Error::other(
                "unsupported Python fullmatch construct in image query",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn actual_caller_patterns_use_equivalent_fullmatch_semantics() {
        for (pattern, positive, negatives) in [
            (
                r"nix/store/[a-z0-9]{32}-nix\.conf/etc/nix/nix\.conf",
                "nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-nix.conf/etc/nix/nix.conf",
                vec![
                    "nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-nixXconf/etc/nix/nix.conf",
                    "nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-nix.conf/etc/nix/nix.conf\n",
                ],
            ),
            (
                r"nix/var/nix/db/db\.sqlite",
                "nix/var/nix/db/db.sqlite",
                vec!["nix/var/nix/db/dbXsqlite", "nix/var/nix/db/db.sqlite\n"],
            ),
            (
                "etc/removed",
                "etc/removed",
                vec!["prefix/etc/removed", "etc/removed/suffix"],
            ),
        ] {
            validate(pattern).unwrap();
            let expression =
                regex::bytes::Regex::new(&format!("(?-u)\\A(?:{pattern})\\z")).unwrap();
            assert!(expression.is_match(positive.as_bytes()));
            for negative in negatives {
                assert!(!expression.is_match(negative.as_bytes()));
            }
        }
    }

    #[test]
    fn unported_python_constructs_fail_explicitly_instead_of_changing_meaning() {
        for pattern in [
            r"(?P<x>a)(?P=x)",
            r"\w+",
            r"a$",
            r"(?i)a",
            r"a.*",
            r"[a-z]",
            "é",
        ] {
            assert!(validate(pattern).is_err());
        }
    }
}
