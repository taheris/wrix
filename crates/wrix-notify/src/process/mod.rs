use std::{
    io::{self, Read, Seek, SeekFrom},
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

impl Runner for Host {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, Error> {
        execute(Command::new(program).args(args), Duration::from_secs(3))
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
        stderr.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes)?;
        return Err(Error::Status(
            status,
            String::from_utf8_lossy(&bytes).into_owned(),
        ));
    }
    stdout.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    stdout.read_to_end(&mut bytes)?;
    Ok(String::from_utf8(bytes)?.trim().to_owned())
}
