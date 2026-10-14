use std::{
    ffi::OsString,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use displaydoc::Display;
use thiserror::Error;
use wait_timeout::ChildExt;

#[derive(Debug, Display, Error)]
pub enum Error {
    /// command I/O failed: {0}
    Io(#[from] io::Error),
    /// command exceeded its deadline
    Timeout,
    /// command failed with status {0}: {1}
    Status(std::process::ExitStatus, String),
    /// command returned invalid UTF-8: {0}
    Utf8(#[from] std::string::FromUtf8Error),
}

pub trait Runner {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, Error>;
}

pub struct Host;

/// GUI callers need neither a login shell nor a CLI symlink in their PATH.
pub fn executable(
    program: &str,
    variable: impl Fn(&str) -> Option<OsString>,
    exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    if program == "omniwmctl" {
        if let Some(path) = variable("WRIX_OMNIWMCTL").filter(|path| !path.is_empty()) {
            return path.into();
        }
        let mut candidates = vec![PathBuf::from(
            "/Applications/OmniWM.app/Contents/MacOS/omniwmctl",
        )];
        if let Some(home) = variable("HOME") {
            candidates
                .push(PathBuf::from(home).join("Applications/OmniWM.app/Contents/MacOS/omniwmctl"));
        }
        if let Some(path) = candidates.into_iter().find(|path| exists(path)) {
            return path;
        }
    }
    if program == "ps" {
        return "/bin/ps".into();
    }
    if program == "lsof" {
        return "/usr/sbin/lsof".into();
    }
    program.into()
}

impl Runner for Host {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, Error> {
        let executable = executable(program, |name| std::env::var_os(name), Path::exists);
        execute(Command::new(executable).args(args), Duration::from_secs(3))
    }
}

/// Captured output cannot deadlock on full pipes or inherited child descriptors.
pub fn execute(command: &mut Command, timeout: Duration) -> Result<String, Error> {
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .spawn()?;
    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        result => {
            if let Err(error) = child.kill() {
                tracing::warn!(%error, pid = child.id(), "failed to terminate command");
            }
            child.wait()?;
            return match result {
                Err(error) => Err(error.into()),
                _ => Err(Error::Timeout),
            };
        }
    };
    if !status.success() {
        stdout.seek(SeekFrom::Start(0))?;
        let mut output = String::new();
        stdout.read_to_string(&mut output)?;
        stderr.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes)?;
        return Err(Error::Status(
            status,
            format!("{output}{}", String::from_utf8_lossy(&bytes)),
        ));
    }
    stdout.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    stdout.read_to_end(&mut bytes)?;
    Ok(String::from_utf8(bytes)?.trim().to_owned())
}
