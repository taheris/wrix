//! Native build-time helper command dispatch.

use crate::{digest, layout, roots};
use std::{
    ffi::OsString,
    fs,
    io::{self, Read, Write},
    path::PathBuf,
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// usage: wrix-image-tools config-digest | configuration-roots REFERENCES CLOSURE OUTPUT SOURCES | oci-layout CONF OUTPUT [OPTIONS] | docker-archive LAYOUT CONF [OPTIONS] | docker-stream CONF [OPTIONS]
    Usage,
    /// image-tool I/O failed: {0}
    Io(#[from] io::Error),
    /// {0}
    Digest(#[from] digest::Error),
    /// {0}
    Roots(#[from] roots::Error),
    /// {0}
    Layout(#[from] layout::Error),
}

enum Command {
    ConfigDigest,
    OciLayout(layout::Options),
    DockerArchive {
        layout: PathBuf,
        conf: PathBuf,
        parent: Option<PathBuf>,
    },
    DockerStream {
        conf: PathBuf,
        tag: Option<String>,
    },
    ConfigurationRoots {
        references: PathBuf,
        closure: PathBuf,
        output: PathBuf,
        sources: PathBuf,
    },
}

impl Command {
    fn docker_stream(args: &[OsString]) -> Result<Self, Error> {
        let mut args = args.iter();
        let mut conf = None;
        let mut tag = None;
        let mut positional = false;
        while let Some(argument) = args.next() {
            if !positional && argument == "--" {
                positional = true;
            } else if !positional && (argument == "--repo_tag" || argument == "-t") {
                tag = Some(
                    args.next()
                        .and_then(|value| value.to_str())
                        .ok_or(Error::Usage)?
                        .to_owned(),
                );
            } else if !positional
                && argument
                    .to_str()
                    .is_some_and(|value| value.starts_with("--repo_tag="))
            {
                tag = Some(
                    argument
                        .to_str()
                        .and_then(|value| value.strip_prefix("--repo_tag="))
                        .ok_or(Error::Usage)?
                        .to_owned(),
                );
            } else if conf.is_none()
                && (positional || argument == "-" || !argument.as_encoded_bytes().starts_with(b"-"))
            {
                conf = Some(PathBuf::from(argument));
            } else {
                return Err(Error::Usage);
            }
        }
        Ok(Self::DockerStream {
            conf: conf.ok_or(Error::Usage)?,
            tag,
        })
    }

    fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, Error> {
        let args: Vec<_> = args.into_iter().collect();
        match args.as_slice() {
            [name] if name == "config-digest" => Ok(Self::ConfigDigest),
            [name, args @ ..] if name == "docker-stream" => Self::docker_stream(args),
            [name, layout, conf] if name == "docker-archive" => Ok(Self::DockerArchive {
                layout: layout.into(),
                conf: conf.into(),
                parent: None,
            }),
            [name, layout, conf, flag, parent]
                if name == "docker-archive" && flag == "--from-layout" =>
            {
                Ok(Self::DockerArchive {
                    layout: layout.into(),
                    conf: conf.into(),
                    parent: Some(parent.into()),
                })
            }
            [name, options @ ..] if name == "oci-layout" => {
                Ok(Self::OciLayout(layout::Options::parse(options)?))
            }
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
        Command::OciLayout(options) => layout::build(&options)?,
        Command::DockerStream { conf, tag } => {
            layout::docker::configured(&conf, tag.as_deref(), &mut io::stdout().lock())?;
        }
        Command::DockerArchive {
            layout: source,
            conf,
            parent,
        } => layout::docker::stream(
            &source,
            &conf,
            parent.as_deref(),
            None,
            &mut io::stdout().lock(),
        )?,
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
    fn docker_stream_keeps_tag_override_order_and_explicit_positional_paths() {
        for values in [
            vec!["docker-stream", "conf", "-t", "alternate:tag"],
            vec!["docker-stream", "--repo_tag", "alternate:tag", "conf"],
            vec!["docker-stream", "--repo_tag=alternate:tag", "conf"],
        ] {
            let Command::DockerStream { conf, tag } =
                Command::parse(values.into_iter().map(OsString::from)).unwrap()
            else {
                panic!("wrong command");
            };
            assert_eq!(conf, PathBuf::from("conf"));
            assert_eq!(tag.as_deref(), Some("alternate:tag"));
        }
        assert!(matches!(
            Command::parse(["docker-stream", "--", "-conf"].map(OsString::from)),
            Ok(Command::DockerStream { tag: None, .. })
        ));
        for values in [
            vec!["docker-stream"],
            vec!["docker-stream", "conf", "--unknown"],
            vec!["docker-stream", "conf", "-t"],
            vec!["docker-stream", "one", "two"],
        ] {
            assert!(Command::parse(values.into_iter().map(OsString::from)).is_err());
        }
    }

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
