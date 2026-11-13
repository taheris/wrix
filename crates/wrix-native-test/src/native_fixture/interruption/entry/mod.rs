use super::{Error, Options, witness};
use std::{ffi::OsString, path::PathBuf, time::Duration};

enum Mode {
    Run,
    Witness,
}
enum Request {
    Run(Options),
    Witness {
        root: PathBuf,
        tools: Option<PathBuf>,
    },
}
fn duration(value: &OsString) -> Result<Duration, Error> {
    let value: f64 = value
        .to_str()
        .ok_or(Error::Policy("timeouts must be finite and positive"))?
        .parse()
        .map_err(|_| Error::Policy("timeouts must be finite and positive"))?;
    if !value.is_finite() || value <= 0.0 {
        return Err(Error::Policy("timeouts must be finite and positive"));
    }
    Duration::try_from_secs_f64(value)
        .map_err(|_| Error::Policy("timeouts must be finite and positive"))
}
fn parse(args: &[OsString]) -> Result<Request, Error> {
    let mut mode = Mode::Run;
    let mut tools = None;
    let mut root = None;
    let mut revision = "dirty".to_owned();
    let mut output = None;
    let mut ready_timeout = Duration::from_secs(600);
    let mut stop_timeout = Duration::from_secs(180);
    let mut command = vec![];
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--witness") => mode = Mode::Witness,
            Some("--tools") => {
                tools = Some(PathBuf::from(
                    args.next()
                        .ok_or(Error::Policy("--tools requires a directory"))?,
                ));
            }
            Some("--root") => {
                root = Some(PathBuf::from(
                    args.next()
                        .ok_or(Error::Policy("--root requires a directory"))?,
                ));
            }
            Some("--output") => {
                output = Some(PathBuf::from(
                    args.next()
                        .ok_or(Error::Policy("--output requires a directory"))?,
                ));
            }
            Some("--source-revision") => {
                args.next()
                    .and_then(|value| value.to_str())
                    .ok_or(Error::Policy("--source-revision requires a revision"))?
                    .clone_into(&mut revision);
            }
            Some("--ready-timeout") => {
                ready_timeout = duration(
                    args.next()
                        .ok_or(Error::Policy("--ready-timeout requires a duration"))?,
                )?;
            }
            Some("--stop-timeout") => {
                stop_timeout = duration(
                    args.next()
                        .ok_or(Error::Policy("--stop-timeout requires a duration"))?,
                )?;
            }
            Some("--") => {
                command.extend(args.cloned());
                break;
            }
            _ => return Err(Error::Policy("unknown native interruption argument")),
        }
    }
    match mode {
        Mode::Witness => Ok(Request::Witness {
            root: root.ok_or(Error::Policy("namespace witness requires a fixture root"))?,
            tools,
        }),
        Mode::Run => Ok(Request::Run(Options {
            tools,
            revision,
            output,
            ready_timeout,
            stop_timeout,
            command,
        })),
    }
}
pub fn run(args: &[OsString]) -> Result<(), Error> {
    match parse(args)? {
        Request::Witness { root, tools } => witness::serve(&root, tools),
        Request::Run(options) => {
            if super::run(&options)? {
                Ok(())
            } else {
                Err(Error::Policy(
                    "native interruption acceptance failed; evidence retained",
                ))
            }
        }
    }
}
#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn non_finite_non_positive_and_unrepresentable_timeouts_are_refused() {
        for value in ["NaN", "inf", "-inf", "0", "-1", "1e300", "no"] {
            assert!(duration(&value.into()).is_err());
        }
        assert_eq!(duration(&"0.5".into()).unwrap(), Duration::from_millis(500));
    }
    #[test]
    fn witness_requires_explicit_root_and_unknown_flags_are_not_commands() {
        assert!(parse(&["--witness".into()]).is_err());
        assert!(parse(&["--unknown".into()]).is_err());
        let Request::Witness { root, .. } =
            parse(&["--witness".into(), "--root".into(), "/fixture".into()]).unwrap()
        else {
            panic!("expected witness")
        };
        assert_eq!(root, PathBuf::from("/fixture"));
    }
}
