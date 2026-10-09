use std::{
    fs, io,
    io::Write,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::ExitStatus,
    time::Instant,
};

use displaydoc::Display;
use serde::{Deserialize, Deserializer, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{FocusTarget, Kind};
use crate::command::config::{AgentKind, BeadId};

#[derive(Debug, Display, Error)]
pub enum Error {
    /// execution metadata I/O failed: {source}
    Io { source: io::Error },
    /// execution metadata serialization failed: {source}
    Json { source: serde_json::Error },
    /// execution metadata timestamp failed: {source}
    Timestamp { source: time::error::Format },
    /// execution metadata filename is invalid
    InvalidFilename,
}

impl From<io::Error> for Error {
    fn from(source: io::Error) -> Self {
        Self::Io { source }
    }
}

#[derive(Serialize)]
#[serde(transparent)]
struct ExecutionId(String);

impl ExecutionId {
    fn parse(filename: &str) -> Result<Self, Error> {
        let value = filename
            .strip_suffix(".json")
            .ok_or(Error::InvalidFilename)?;
        if value.is_empty()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(Error::InvalidFilename);
        }
        Ok(Self(value.to_owned()))
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Run,
    Spawn,
}

impl From<&Kind> for Mode {
    fn from(kind: &Kind) -> Self {
        match kind {
            Kind::Run(_) => Self::Run,
            Kind::Spawn(_) => Self::Spawn,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
enum State {
    Incomplete {
        timestamp_end: (),
        duration_seconds: (),
        exit_code: (),
        signal: (),
    },
    Completed {
        timestamp_end: String,
        duration_seconds: f64,
        exit_code: Option<i32>,
        signal: Option<String>,
    },
}

#[derive(Serialize)]
struct Record {
    execution_id: ExecutionId,
    timestamp_start: String,
    mode: Mode,
    agent_kind: AgentKind,
    bead_id: Option<BeadId>,
    focus_target: Option<FocusTarget>,
    agent_session_id: Option<ConversationId>,
    agent_session_dir: Option<&'static Path>,
    #[serde(flatten)]
    state: State,
}

/// Conversation identity is unavailable without execution-attributable runtime reporting.
#[derive(Serialize)]
#[serde(transparent)]
struct ConversationId(String);

impl<'de> Deserialize<'de> for ConversationId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(serde::de::Error::custom(
                "invalid agent conversation identifier",
            ));
        }
        Ok(Self(value))
    }
}

pub struct Journal {
    path: PathBuf,
    start: Instant,
    record: Record,
}

impl Journal {
    pub fn create(
        workspace: &Path,
        kind: &Kind,
        agent_kind: AgentKind,
        focus_target: Option<&FocusTarget>,
    ) -> Result<Self, Error> {
        let start = Instant::now();
        let timestamp_start = timestamp()?;
        let directory = workspace.join(".wrix/log");
        fs::create_dir_all(&directory)?;
        let prefix = format!(".{}-", timestamp_start.replace([':', '.'], "-"));
        let mut temporary = tempfile::Builder::new()
            .prefix(&prefix)
            .suffix(".json")
            .tempfile_in(&directory)?;
        let filename = temporary
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix('.'))
            .ok_or(Error::InvalidFilename)?;
        let path = directory.join(filename);
        let record = Record {
            execution_id: ExecutionId::parse(filename)?,
            timestamp_start,
            mode: Mode::from(kind),
            agent_kind,
            bead_id: match kind {
                Kind::Run(_) => None,
                Kind::Spawn(spawn) => spawn.config.bead_id.clone(),
            },
            focus_target: focus_target.cloned(),
            agent_session_id: None,
            agent_session_dir: match agent_kind {
                AgentKind::Claude => Some(Path::new("/workspace/.claude")),
                AgentKind::Pi => Some(Path::new("/workspace/.pi/agent/sessions")),
                AgentKind::Direct => None,
            },
            state: State::Incomplete {
                timestamp_end: (),
                duration_seconds: (),
                exit_code: (),
                signal: (),
            },
        };
        write_record(&mut temporary, &record)?;
        temporary
            .persist_noclobber(&path)
            .map_err(|error| Error::Io {
                source: error.error,
            })?;
        Ok(Self {
            path,
            start,
            record,
        })
    }

    /// Only an observed foreground runtime wait result supplies status fields.
    pub fn complete(mut self, status: Option<ExitStatus>) -> Result<(), Error> {
        self.record.state = State::Completed {
            timestamp_end: timestamp()?,
            duration_seconds: self.start.elapsed().as_secs_f64(),
            exit_code: status.and_then(|status| status.code()),
            signal: status.and_then(|status| status.signal()).map(signal_name),
        };
        let directory = self.path.parent().ok_or(Error::InvalidFilename)?;
        let mut temporary = NamedTempFile::new_in(directory)?;
        write_record(&mut temporary, &self.record)?;
        temporary.persist(&self.path).map_err(|error| Error::Io {
            source: error.error,
        })?;
        Ok(())
    }
}

fn timestamp() -> Result<String, Error> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|source| Error::Timestamp { source })
}

fn write_record(file: &mut NamedTempFile, record: &Record) -> Result<(), Error> {
    let bytes = serde_json::to_vec(record).map_err(|source| Error::Json { source })?;
    file.write_all(&bytes)?;
    file.flush()?;
    Ok(())
}

fn signal_name(signal: i32) -> String {
    let name = match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGABRT => "SIGABRT",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGCHLD => "SIGCHLD",
        libc::SIGCONT => "SIGCONT",
        libc::SIGSTOP => "SIGSTOP",
        libc::SIGTSTP => "SIGTSTP",
        libc::SIGTTIN => "SIGTTIN",
        libc::SIGTTOU => "SIGTTOU",
        _ => return format!("SIG{signal}"),
    };
    name.to_owned()
}
