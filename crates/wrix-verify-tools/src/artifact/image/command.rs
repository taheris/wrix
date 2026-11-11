//! Parsing for the emitted image inspector's command surface.

use std::{ffi::OsString, io};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// usage: wrix-verify-tools image-artifact OPERATION ARGS
    Usage,
    /// image inspection I/O failed: {0}
    Io(#[from] io::Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Operation {
    Prepare,
    Paths,
    Digests,
    Extract,
    Exists,
    Link,
    Find,
    Help,
}

impl Operation {
    pub(super) fn parse(args: &[OsString]) -> Result<Self, Error> {
        let Some(first) = args.first() else {
            return Err(Error::Usage);
        };
        let operation = match first.to_str() {
            Some("prepare") => Self::Prepare,
            Some("paths") => Self::Paths,
            Some("digests") => Self::Digests,
            Some("extract") => Self::Extract,
            Some("exists") => Self::Exists,
            Some("link") => Self::Link,
            Some("find") => Self::Find,
            Some("--help" | "-h") => Self::Help,
            _ => return Err(Error::Usage),
        };
        if operation == Self::Help || args[1..].iter().any(|arg| arg == "--help" || arg == "-h") {
            return Ok(Self::Help);
        }
        let count = match operation {
            Self::Prepare | Self::Paths | Self::Digests => 3,
            Self::Extract => 5,
            Self::Exists | Self::Link | Self::Find => 4,
            Self::Help => 1,
        };
        if args.len() != count {
            return Err(Error::Usage);
        }
        Ok(operation)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn missing_extra_and_unknown_arguments_fail_before_any_image_is_opened() {
        for args in [
            vec![],
            vec!["prepare"],
            vec!["exists", "image", "layers"],
            vec!["paths", "image", "layers", "extra"],
            vec!["unknown"],
        ] {
            let args = args.into_iter().map(OsString::from).collect::<Vec<_>>();
            assert!(matches!(Operation::parse(&args), Err(Error::Usage)));
        }
    }

    #[test]
    fn help_does_not_require_an_image_or_open_payloads() {
        for args in [
            vec!["--help"],
            vec!["extract", "--help"],
            vec!["prepare", "missing", "--help"],
        ] {
            let args = args.into_iter().map(OsString::from).collect::<Vec<_>>();
            assert_eq!(Operation::parse(&args).unwrap(), Operation::Help);
        }
    }
}
