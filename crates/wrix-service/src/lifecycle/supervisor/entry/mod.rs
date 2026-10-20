//! Internal image entry point; namespace clients cannot select or replace child commands.

use std::{ffi::OsString, io::Write, path::PathBuf};

use super::{Action, Error, Observation, client, serve};
use crate::control::Namespace;

enum Invocation {
    Serve {
        root: PathBuf,
        namespace: Namespace,
        socket: PathBuf,
        first: String,
        second: String,
    },
    Request {
        root: PathBuf,
        namespace: Namespace,
        action: Action,
    },
}

impl Invocation {
    fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Self, Error> {
        let mode = args.next().ok_or(Error::Arguments)?;
        let root = PathBuf::from(args.next().ok_or(Error::Arguments)?);
        let namespace = Namespace::try_from(
            args.next()
                .ok_or(Error::Arguments)?
                .into_string()
                .map_err(|_| Error::Arguments)?,
        )
        .map_err(|_| Error::Incompatible)?;
        let invocation = match mode.to_str() {
            Some("serve") => Self::Serve {
                root,
                namespace,
                socket: PathBuf::from(args.next().ok_or(Error::Arguments)?),
                first: args
                    .next()
                    .ok_or(Error::Arguments)?
                    .into_string()
                    .map_err(|_| Error::Arguments)?,
                second: args
                    .next()
                    .ok_or(Error::Arguments)?
                    .into_string()
                    .map_err(|_| Error::Arguments)?,
            },
            Some(mode @ ("inspect" | "retire" | "resume")) => {
                let action = match mode {
                    "inspect" => Action::Inspect,
                    "retire" => Action::Retire,
                    "resume" => Action::Resume,
                    _ => return Err(Error::Arguments),
                };
                Self::Request {
                    root,
                    namespace,
                    action,
                }
            }
            _ => return Err(Error::Arguments),
        };
        if args.next().is_some() {
            return Err(Error::Arguments);
        }
        Ok(invocation)
    }
}

pub fn run() -> Result<(), Error> {
    let invocation = Invocation::parse(std::env::args_os().skip(1))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let observation = runtime.block_on(async {
        match invocation {
            Invocation::Serve {
                root,
                namespace,
                socket,
                first,
                second,
            } => {
                let mut terminate =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
                let mut interrupt =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
                serve::run(&root, namespace, &socket, &first, &second, async {
                    tokio::select! { _ = terminate.recv() => (), _ = interrupt.recv() => () }
                })
                .await?;
                Ok(None)
            }
            Invocation::Request {
                root,
                namespace,
                action,
            } => {
                let observation = if action == Action::Retire {
                    client::retire(&root, &namespace).await?;
                    Observation::Retired
                } else {
                    client::request(&root, &namespace, action).await?
                };
                Ok::<_, Error>(Some(observation))
            }
        }
    })?;
    if let Some(observation) = observation {
        let mut output = serde_json::to_vec(&observation)?;
        output.push(b'\n');
        std::io::stdout().write_all(&output)?;
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    fn namespace() -> String {
        "a".repeat(64)
    }

    #[test]
    fn malformed_or_extra_internal_arguments_never_become_an_operation() {
        for args in [
            vec!["inspect".into(), "/data".into(), "invalid".into()],
            vec!["retire".into(), "/data".into(), namespace(), "extra".into()],
            vec!["serve".into(), "/data".into(), namespace(), "first".into()],
            vec!["shutdown".into(), "/data".into(), namespace()],
        ] {
            assert!(Invocation::parse(args.into_iter().map(OsString::from)).is_err());
        }
    }
}
