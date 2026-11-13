use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io::{self, Read, Write},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// native fixture subprocess I/O failed: {0}
    Io(#[from] io::Error),
    /// native fixture descriptor setup failed: {0}
    Descriptor(#[from] rustix::io::Errno),
    /// native fixture command exceeded its deadline
    Timeout,
    /// native fixture command output exceeded its bound
    Oversized,
    /// native fixture command failed ({status}): {message}
    Failed { status: ExitStatus, message: String },
}

pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

const LIMIT: usize = 8 * 1024 * 1024;

fn drain(reader: &mut impl Read, output: &mut Vec<u8>) -> Result<bool, Error> {
    let mut buffer = [0; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => {
                if count > LIMIT - output.len() {
                    return Err(Error::Oversized);
                }
                output.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
}

pub fn capture(command: &mut Command, timeout: Duration) -> Result<Output, Error> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let result = (|| {
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing child stdout"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("missing child stderr"))?;
        rustix::fs::fcntl_setfl(&stdout, rustix::fs::OFlags::NONBLOCK)?;
        rustix::fs::fcntl_setfl(&stderr, rustix::fs::OFlags::NONBLOCK)?;
        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut status = None;
        loop {
            let stdout_done = drain(&mut stdout, &mut out)?;
            let stderr_done = drain(&mut stderr, &mut err)?;
            if status.is_none() {
                status = child.try_wait()?;
            }
            if let Some(status) = status.filter(|_| stdout_done && stderr_done) {
                return Ok(Output {
                    status,
                    stdout: out,
                    stderr: err,
                });
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout);
            }
            thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() && child.try_wait()?.is_none() {
        child.kill()?;
        child.wait()?;
    }
    result
}

pub fn checked(command: &mut Command, timeout: Duration) -> Result<Vec<u8>, Error> {
    let output = capture(command, timeout)?;
    if !output.status.success() {
        return Err(Error::Failed {
            status: output.status,
            message: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    if !output.stderr.is_empty() {
        io::stderr().lock().write_all(&output.stderr)?;
    }
    Ok(output.stdout)
}

pub struct Environment {
    values: BTreeMap<OsString, OsString>,
}
impl Environment {
    pub fn private(root: &std::path::Path) -> Self {
        let mut values: BTreeMap<_, _> = std::env::vars_os().collect();
        for key in [
            "CONTAINER_HOST",
            "CONTAINER_CONNECTION",
            "CONTAINERS_STORAGE_CONF",
            "CONTAINERS_CONF",
            "CONTAINERS_CONF_OVERRIDE",
            "CONTAINERS_CONF_MODULES",
            "XDG_DATA_DIRS",
            "XDG_CONFIG_DIRS",
        ] {
            values.remove(OsStr::new(key));
        }
        for (key, relative) in [
            ("HOME", "home"),
            ("XDG_DATA_HOME", "home/.local/share"),
            ("XDG_CONFIG_HOME", "home/.config"),
            ("XDG_STATE_HOME", "home/.local/state"),
            ("XDG_RUNTIME_DIR", "runtime"),
        ] {
            values.insert(key.into(), root.join(relative).into_os_string());
        }
        Self { values }
    }
    pub fn set(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) {
        self.values.insert(key.into(), value.into());
    }
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        command.env_clear().envs(&self.values);
        command
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn concurrent_stdout_and_stderr_do_not_deadlock_capture() {
        let output = capture(
            Command::new("bash").args([
                "-c",
                "for ((i=0;i<10000;i++)); do printf 'out\\n'; printf 'err\\n' >&2; done",
            ]),
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 40000);
        assert_eq!(output.stderr.len(), 40000);
    }
    #[test]
    fn failing_command_preserves_status_and_stderr() {
        let error = checked(
            Command::new("bash").args(["-c", "printf 'owned failure' >&2; exit 23"]),
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(
            matches!(error, Error::Failed { status, message } if status.code() == Some(23) && message == "owned failure")
        );
    }
    #[test]
    fn hung_direct_child_is_stopped_within_its_bound() {
        let start = Instant::now();
        assert!(matches!(
            capture(Command::new("sleep").arg("60"), Duration::from_millis(100)),
            Err(Error::Timeout)
        ));
        assert!(start.elapsed() < Duration::from_secs(5));
    }
    #[test]
    fn environment_uses_only_fixture_storage_and_explicit_overrides() {
        let mut environment = Environment::private(std::path::Path::new("/owned"));
        environment.set("CONTAINER_HOST", "fixture-only");
        let command = environment.command("podman");
        let values: BTreeMap<_, _> = command
            .get_envs()
            .map(|(key, value)| (key, value.unwrap()))
            .collect();
        assert_eq!(values[OsStr::new("HOME")], OsStr::new("/owned/home"));
        assert_eq!(
            values[OsStr::new("XDG_RUNTIME_DIR")],
            OsStr::new("/owned/runtime")
        );
        assert_eq!(
            values[OsStr::new("CONTAINER_HOST")],
            OsStr::new("fixture-only")
        );
        assert!(!values.contains_key(OsStr::new("CONTAINERS_CONF_OVERRIDE")));
    }
}
