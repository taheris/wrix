use super::{Error, Runtime, Subject, discover};
use crate::native_fixture::root;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy)]
enum Discovery {
    Explicit,
    IncludeOwned,
}
#[derive(Clone, Copy)]
enum Files {
    Retain,
    Remove,
}
#[derive(Clone, Copy)]
enum Diagnostics {
    Quiet,
    Capture,
}
struct Selection {
    roots: BTreeSet<PathBuf>,
    registry: Option<PathBuf>,
    tools: Option<PathBuf>,
    discovery: Discovery,
    files: Files,
    diagnostics: Diagnostics,
}
enum Request {
    Validate(PathBuf),
    Signal { pid: u32, parent: u32 },
    Cleanup(Selection),
}

fn parse(args: &[OsString]) -> Result<Request, Error> {
    let mut roots = BTreeSet::new();
    let mut registry = None;
    let mut tools = None;
    let mut discovery = Discovery::Explicit;
    let mut files = Files::Retain;
    let mut diagnostics = Diagnostics::Quiet;
    let mut signal = None;
    let mut parent = None;
    let mut validation = None;
    let mut args = args.iter();
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--discover") => discovery = Discovery::IncludeOwned,
            Some("--remove-files") => files = Files::Remove,
            Some("--diagnostics") => diagnostics = Diagnostics::Capture,
            Some("--registry") => {
                registry = Some(PathBuf::from(
                    args.next()
                        .ok_or(Error::Policy("--registry requires a directory"))?,
                ));
            }
            Some("--validate-registry") => {
                validation = Some(PathBuf::from(
                    args.next()
                        .ok_or(Error::Policy("--validate-registry requires a directory"))?,
                ));
            }
            Some("--tools") => {
                tools = Some(PathBuf::from(
                    args.next()
                        .ok_or(Error::Policy("--tools requires a directory"))?,
                ));
            }
            Some("--signal-child") => signal = Some(pid(args.next())?),
            Some("--parent") => parent = Some(pid(args.next())?),
            Some("--") => {
                roots.extend(args.map(PathBuf::from));
                break;
            }
            _ if argument.as_encoded_bytes().starts_with(b"-") => {
                return Err(Error::Policy("unknown native fixture cleanup option"));
            }
            _ => {
                roots.insert(PathBuf::from(argument));
            }
        }
    }
    if let Some(pid) = signal {
        return Ok(Request::Signal {
            pid,
            parent: parent.ok_or(Error::Policy("--signal-child requires its owning --parent"))?,
        });
    }
    if parent.is_some() {
        return Err(Error::Policy("--parent requires --signal-child"));
    }
    if let Some(registry) = validation {
        return Ok(Request::Validate(registry));
    }
    if roots.is_empty() && registry.is_none() && matches!(discovery, Discovery::Explicit) {
        return Err(Error::Policy(
            "specify fixture roots, --registry or --discover",
        ));
    }
    Ok(Request::Cleanup(Selection {
        roots,
        registry,
        tools,
        discovery,
        files,
        diagnostics,
    }))
}
fn pid(value: Option<&OsString>) -> Result<u32, Error> {
    let value = value
        .and_then(|value| value.to_str())
        .ok_or(Error::Policy("missing native child identity"))?
        .parse()
        .map_err(|_| Error::Policy("invalid native child identity"))?;
    if value == 0 {
        return Err(Error::Policy("invalid native child identity"));
    }
    Ok(value)
}

pub fn run(args: &[OsString]) -> Result<(), Error> {
    #[cfg(target_os = "linux")]
    {
        run_with(args, &mut super::Native)
    }
    #[cfg(not(target_os = "linux"))]
    {
        parse(args)?;
        Err(Error::Policy("native fixture cleanup requires Linux"))
    }
}

pub fn run_with(args: &[OsString], runtime: &mut impl Runtime) -> Result<(), Error> {
    let mut selection = match parse(args)? {
        Request::Validate(path) => {
            root::registry(&path)?;
            return Ok(());
        }
        Request::Signal { pid, parent } => {
            #[cfg(target_os = "linux")]
            crate::native_fixture::process::signal_child(pid, parent)?;
            #[cfg(not(target_os = "linux"))]
            {
                return Err(Error::SignalPlatform { pid, parent });
            }
            #[cfg(target_os = "linux")]
            return Ok(());
        }
        Request::Cleanup(selection) => selection,
    };
    let entries = match &selection.registry {
        Some(path) => root::registry(path)?,
        None => BTreeMap::new(),
    };
    selection.roots.extend(entries.keys().cloned());
    if matches!(selection.discovery, Discovery::IncludeOwned) {
        selection
            .roots
            .extend(discover(runtime, Path::new("/proc"), Path::new("/tmp"))?);
    }
    let mut failed = false;
    for path in selection.roots {
        writeln!(
            io::stdout().lock(),
            "cleaning native test fixture: {}",
            path.display()
        )?;
        let result = (|| {
            let origin = if entries.contains_key(&path) {
                root::Origin::Registered
            } else {
                root::Origin::Discovered
            };
            let subject = Subject::new(root::Root::parse(&path, origin)?, selection.tools.clone())?;
            subject.finish(
                runtime,
                matches!(selection.diagnostics, Diagnostics::Capture),
            )?;
            if matches!(selection.files, Files::Remove) {
                subject.remove_files(runtime)?;
                if let Some(entry) = entries.get(&path) {
                    fs::remove_file(entry)?;
                }
                writeln!(
                    io::stdout().lock(),
                    "native test resources and fixture files removed: {}",
                    path.display()
                )?;
            } else {
                writeln!(
                    io::stdout().lock(),
                    "native test resources stopped and unmounted; files retained: {}",
                    path.display()
                )?;
            }
            Ok::<_, Error>(())
        })();
        if let Err(error) = result {
            failed = true;
            writeln!(
                io::stderr().lock(),
                "cleanup incomplete; retain {}: {error}",
                path.display()
            )?;
        }
    }
    if failed {
        Err(Error::Policy(
            "one or more native fixtures could not be safely cleaned",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn incomplete_or_unknown_cli_arguments_fail_before_cleanup() {
        for args in [
            vec![],
            vec!["--unknown"],
            vec!["--registry"],
            vec!["--signal-child", "0", "--parent", "1"],
            vec!["--signal-child", "2"],
            vec!["--parent", "1"],
        ] {
            assert!(parse(&args.into_iter().map(OsString::from).collect::<Vec<_>>()).is_err());
        }
    }
    #[test]
    fn path_arguments_are_not_mistaken_for_flags_after_separator() {
        let Request::Cleanup(selection) = parse(&["--".into(), "--fixture".into()]).unwrap() else {
            panic!("expected explicit roots")
        };
        assert_eq!(
            selection.roots,
            BTreeSet::from([PathBuf::from("--fixture")])
        );
    }
}
