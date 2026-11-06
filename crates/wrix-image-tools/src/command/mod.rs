//! Native build-time helper command dispatch.

use crate::{digest, roots};
use std::{
    ffi::OsString,
    fs,
    io::{self, Read, Write},
    path::PathBuf,
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// usage: wrix-image-tools config-digest | configuration-roots REFERENCES CLOSURE OUTPUT SOURCES
    Usage,
    /// image-tool I/O failed: {0}
    Io(#[from] io::Error),
    /// {0}
    Digest(#[from] digest::Error),
    /// {0}
    Roots(#[from] roots::Error),
}

enum Command {
    ConfigDigest,
    ConfigurationRoots {
        references: PathBuf,
        closure: PathBuf,
        output: PathBuf,
        sources: PathBuf,
    },
}

impl Command {
    fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, Error> {
        let args: Vec<_> = args.into_iter().collect();
        match args.as_slice() {
            [name] if name == "config-digest" => Ok(Self::ConfigDigest),
            [name, references, closure, output, sources] if name == "configuration-roots" => {
                Ok(Self::ConfigurationRoots {
                    references: references.into(),
                    closure: closure.into(),
                    output: output.into(),
                    sources: sources.into(),
                })
            }
            _ => Err(Error::Usage),
        }
    }
}

pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<(), Error> {
    match Command::parse(args)? {
        Command::ConfigDigest => {
            let mut bytes = Vec::new();
            io::stdin().lock().read_to_end(&mut bytes)?;
            writeln!(io::stdout().lock(), "{}", digest::config_digest(&bytes)?)?;
        }
        Command::ConfigurationRoots {
            references,
            closure,
            output,
            sources,
        } => {
            let candidates = roots::parse_references(&fs::read(references)?)?;
            let closure = roots::parse_closure(&fs::read_to_string(closure)?)?;
            let sources = roots::parse_references(&fs::read(sources)?)?;
            roots::publish(&candidates, &closure, &sources, &output)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn arguments_are_exhaustive_and_missing_or_extra_arguments_fail() {
        for args in [
            vec![],
            vec!["other"],
            vec!["config-digest", "extra"],
            vec!["configuration-roots", "a", "b", "c"],
            vec!["configuration-roots", "a", "b", "c", "d", "extra"],
        ] {
            assert!(matches!(
                Command::parse(args.into_iter().map(OsString::from)),
                Err(Error::Usage)
            ));
        }
        assert!(matches!(
            Command::parse([OsString::from("config-digest")]),
            Ok(Command::ConfigDigest)
        ));
        assert!(matches!(
            Command::parse(
                ["configuration-roots", "a", "b", "c", "d"]
                    .into_iter()
                    .map(OsString::from)
            ),
            Ok(Command::ConfigurationRoots { .. })
        ));
    }
}
