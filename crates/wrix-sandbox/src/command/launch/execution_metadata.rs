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
        Self::create_at(
            workspace,
            kind,
            agent_kind,
            focus_target,
            OffsetDateTime::now_utc(),
        )
    }

    fn create_at(
        workspace: &Path,
        kind: &Kind,
        agent_kind: AgentKind,
        focus_target: Option<&FocusTarget>,
        now: OffsetDateTime,
    ) -> Result<Self, Error> {
        let start = Instant::now();
        let timestamp_start = timestamp(now)?;
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
            timestamp_end: timestamp(OffsetDateTime::now_utc())?,
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

fn timestamp(now: OffsetDateTime) -> Result<String, Error> {
    now.format(&Rfc3339)
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

#[cfg(test)]
mod test {
    use std::{fs, os::unix::process::ExitStatusExt, process::ExitStatus, sync::Barrier, thread};

    use serde_json::Value;
    use time::OffsetDateTime;

    use super::Journal;
    use crate::command::{
        config::AgentKind,
        launch::{Kind, Run},
    };

    #[test]
    fn concurrent_same_timestamp_records_remain_distinct() {
        let workspace = tempfile::tempdir().unwrap();
        let kind = Kind::Run(Run {
            workspace: workspace.path().to_owned(),
            agent_args: Vec::new(),
            git_deploy: None,
            git_sign: None,
        });
        let barrier = Barrier::new(2);
        let create = || {
            barrier.wait();
            Journal::create_at(
                workspace.path(),
                &kind,
                AgentKind::Direct,
                None,
                OffsetDateTime::UNIX_EPOCH,
            )
            .unwrap()
        };
        let [a, b] = thread::scope(|scope| {
            let a = scope.spawn(create);
            let b = scope.spawn(create);
            [a.join().unwrap(), b.join().unwrap()]
        });
        let paths = [a.path.clone(), b.path.clone()];
        assert_ne!(paths[0], paths[1]);
        let read = |path| -> Value { serde_json::from_slice(&fs::read(path).unwrap()).unwrap() };
        let initial = paths.each_ref().map(read);
        assert_ne!(initial[0]["execution_id"], initial[1]["execution_id"]);
        for (path, value) in paths.iter().zip(&initial) {
            assert_eq!(value["timestamp_start"], "1970-01-01T00:00:00Z");
            assert_eq!(value["state"], "incomplete");
            let filename = path.file_stem().unwrap().to_str().unwrap();
            assert_eq!(filename, value["execution_id"].as_str().unwrap());
            assert!(filename.starts_with("1970-01-01T00-00-00Z-"));
        }
        a.complete(Some(ExitStatus::from_raw(0))).unwrap();
        let completed_a = read(&paths[0]);
        assert_eq!(completed_a["state"], "completed");
        assert_eq!(completed_a["exit_code"], 0);
        assert_eq!(read(&paths[1]), initial[1]);
        b.complete(Some(ExitStatus::from_raw(37 << 8))).unwrap();
        let completed_b = read(&paths[1]);
        assert_eq!(completed_b["state"], "completed");
        assert_eq!(completed_b["exit_code"], 37);
        assert_eq!(read(&paths[0]), completed_a);
        for (completed, initial) in [completed_a, completed_b].iter().zip(&initial) {
            assert_eq!(completed["execution_id"], initial["execution_id"]);
            assert_eq!(completed["timestamp_start"], initial["timestamp_start"]);
        }
        let mut recorded_paths = fs::read_dir(workspace.path().join(".wrix/log"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        recorded_paths.sort();
        let mut expected_paths = paths;
        expected_paths.sort();
        assert_eq!(recorded_paths, expected_paths);
    }
}
