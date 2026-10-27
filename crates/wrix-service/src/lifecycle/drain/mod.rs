//! Fixed-role native work drain, separate from transport and process-liveness observations.
use displaydoc::Display;
use serde::{Deserialize, Serialize};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Celler,
    Compiler,
}
impl Role {
    pub const fn socket(self) -> &'static str {
        match self {
            Self::Celler => "/run/wrix/celler/drain.sock",
            Self::Compiler => "/run/wrix/compiler/drain.sock",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Begin,
    Quiesce,
    Status,
    Abort,
    Release,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Serving,
    Draining,
    Releasing,
    Released,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub phase: Phase,
    pub requests: usize,
    pub workflows: usize,
    pub background: usize,
    pub failed: bool,
}
impl Status {
    pub const fn drained(&self) -> bool {
        matches!(self.phase, Phase::Draining)
            && self.requests == 0
            && self.workflows == 0
            && self.background == 0
            && !self.failed
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub accepted: bool,
    pub status: Status,
}

#[derive(Debug, Display, Error)]
pub enum Failure {
    /// native drain transport failed: {0}
    Io(#[from] io::Error),
    /// native drain protocol failed: {0}
    Json(#[from] serde_json::Error),
    /// native drain response exceeded its bound
    Oversize,
    /// native role rejected {0:?}; no ownership release is established
    Rejected(Command),
    /// native role has not proven clean release
    Unreleased,
    /// backend is busy; drain was aborted and old admission reopened
    Busy,
    /// native work completion is uncertain; transition was aborted before shutdown
    Uncertain,
    /// drain failed and old admission could not be confirmed reopened: {0}
    Abort(Box<Failure>),
}

pub trait Native {
    fn command(&mut self, command: Command) -> Result<Response, Failure>;
}

/// Container-local only; native container exec may carry its serialized result to the host.
pub struct Client {
    role: Role,
    timeout: Duration,
}
impl Client {
    pub const fn new(role: Role, timeout: Duration) -> Self {
        Self { role, timeout }
    }
}
impl Native for Client {
    fn command(&mut self, command: Command) -> Result<Response, Failure> {
        let mut stream = UnixStream::connect(self.role.socket())?;
        stream.set_read_timeout(if command == Command::Release {
            None
        } else {
            Some(self.timeout)
        })?;
        stream.set_write_timeout(Some(self.timeout))?;
        exchange(&mut stream, command)
    }
}

/// Fixed-role private entry dispatch; release waits for native sync/closure, not a kill deadline.
pub fn dispatch(role: Role, command: Command) -> Result<Response, Failure> {
    Client::new(role, Duration::from_secs(5)).command(command)
}

pub fn exchange(stream: &mut (impl Read + Write), command: Command) -> Result<Response, Failure> {
    #[derive(Serialize)]
    struct Request {
        command: Command,
    }
    let mut bytes = serde_json::to_vec(&Request { command })?;
    bytes.push(b'\n');
    stream.write_all(&bytes)?;
    stream.flush()?;
    let mut response = Vec::new();
    BufReader::new(stream)
        .take(4097)
        .read_until(b'\n', &mut response)?;
    if response.len() > 4096 {
        return Err(Failure::Oversize);
    }
    let response: Response = serde_json::from_slice(&response)?;
    if !response.accepted {
        return Err(Failure::Rejected(command));
    }
    Ok(response)
}

/// Busy budgets are pre-stop only; after release no rollback is authorized.
pub fn quiesce(native: &mut impl Native, budget: Duration) -> Result<Status, Failure> {
    let start = Instant::now();
    let result = (|| {
        let mut status = checked(native, Command::Begin)?;
        loop {
            if status.drained() {
                let quiescent = checked(native, Command::Quiesce)?;
                if !quiescent.drained() {
                    return Err(Failure::Uncertain);
                }
                return Ok(quiescent);
            }
            if status.phase != Phase::Draining || status.failed {
                return Err(Failure::Uncertain);
            }
            if start.elapsed() >= budget {
                return Err(Failure::Busy);
            }
            std::thread::sleep(
                Duration::from_millis(20).min(budget.saturating_sub(start.elapsed())),
            );
            status = checked(native, Command::Status)?;
        }
    })();
    match result {
        Ok(status) => Ok(status),
        Err(error) => {
            match checked(native, Command::Abort) {
                Ok(status) if status.phase == Phase::Serving => (),
                Ok(_) => return Err(Failure::Abort(Box::new(Failure::Unreleased))),
                Err(abort) => return Err(Failure::Abort(Box::new(abort))),
            }
            Err(error)
        }
    }
}

fn checked(native: &mut impl Native, command: Command) -> Result<Status, Failure> {
    let response = native.command(command)?;
    if !response.accepted {
        return Err(Failure::Rejected(command));
    }
    Ok(response.status)
}

/// The supervisor must additionally reap the planned native child and verify zero exit.
pub fn release(native: &mut impl Native) -> Result<Status, Failure> {
    let status = checked(native, Command::Release)?;
    if status.phase != Phase::Released
        || status.requests != 0
        || status.workflows != 0
        || status.background != 0
        || status.failed
    {
        return Err(Failure::Unreleased);
    }
    Ok(status)
}
