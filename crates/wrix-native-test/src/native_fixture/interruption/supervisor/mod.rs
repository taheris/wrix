use super::{Error, Progress, checkpoint::Nonce, write_json};
use crate::native_fixture::process;
use rustix::process::{Pid, Signal, WaitIdOptions};
use std::{
    ffi::OsString,
    fs::{self, File},
    io::Read,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub struct Supervisor {
    child: Child,
    pid: Pid,
    reaped: bool,
    stop_timeout: Duration,
    stop_deadline: Option<Instant>,
    code: Option<i32>,
}
impl Supervisor {
    pub fn spawn(
        invocation: &[OsString],
        nonce: &Nonce,
        log: File,
        stop_timeout: Duration,
    ) -> Result<Self, Error> {
        let child = Command::new("setsid")
            .args(invocation)
            .env("WRIX_TEST_POOL_INTERRUPT_NONCE", nonce.as_str())
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()?;
        let pid = Pid::from_raw(
            i32::try_from(child.id()).map_err(|_| Error::Policy("invalid supervisor identity"))?,
        )
        .ok_or(Error::Policy("invalid supervisor identity"))?;
        Ok(Self {
            child,
            pid,
            reaped: false,
            stop_timeout,
            stop_deadline: None,
            code: None,
        })
    }
    pub fn id(&self) -> u32 {
        self.child.id()
    }
    pub const fn code(&self) -> Option<i32> {
        self.code
    }
    pub fn exited(&mut self) -> Result<Option<i32>, Error> {
        if self.reaped {
            return Ok(self.code);
        }
        if let Some(status) = rustix::process::waitid(
            rustix::process::WaitId::Pid(self.pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )? {
            self.code = Some(match (status.exit_status(), status.terminating_signal()) {
                (Some(code), _) => code,
                (_, Some(signal)) => -signal,
                _ => return Err(Error::Policy("unrecognized native supervisor wait state")),
            });
        }
        Ok(self.code)
    }
    pub fn signal(&mut self) -> Result<(), Error> {
        if self.exited()?.is_some() {
            return Err(Error::Policy(
                "native supervisor exited before the planned signal",
            ));
        }
        self.stop_deadline = Some(Instant::now() + self.stop_timeout);
        rustix::process::kill_process(self.pid, Signal::TERM)?;
        Ok(())
    }
    pub fn capture_helpers(
        &self,
        phase: &str,
        output: &Path,
        progress: &Progress,
    ) -> Result<(), Error> {
        if self.reaped {
            return Ok(());
        }
        let records = diagnostics(self.id())?;
        write_json(&output.join(format!("helpers-{phase}.json")), &records)?;
        for record in records {
            progress.say(format_args!(
                "Owned helper {}: state={} exe={} wchan={}",
                record["pid"], record["state"], record["exe"], record["wchan"]
            ))?;
        }
        Ok(())
    }
    pub fn stop_helpers(
        &mut self,
        deadline: Instant,
        output: &Path,
        progress: &Progress,
    ) -> Result<Vec<u32>, Error> {
        if self.exited()?.is_none() {
            return Err(Error::Policy(
                "cannot reap assertion helpers before supervisor teardown",
            ));
        }
        self.capture_helpers("before-term", output, progress)?;
        let members = process::signal_session(self.id(), Signal::TERM)?;
        if !members.is_empty() {
            progress.say(format_args!(
                "Stopping {} assertion helpers in the owned private session",
                members.len()
            ))?;
        }
        let deadline = deadline.min(Instant::now() + Duration::from_secs(10));
        while !process::session_members(Path::new("/proc"), self.id())?.is_empty()
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(50));
        }
        if !process::session_members(Path::new("/proc"), self.id())?.is_empty() {
            self.capture_helpers("after-term", output, progress)?;
            return Err(Error::Policy(
                "owned assertion helpers did not exit gracefully; see helpers-after-term.json",
            ));
        }
        Ok(members)
    }
    pub fn finish(&mut self) -> Result<i32, Error> {
        if self.exited()?.is_none() {
            return Err(Error::Policy("owned native supervisor has not exited"));
        }
        if !process::session_members(Path::new("/proc"), self.id())?.is_empty() {
            return Err(Error::Policy(
                "native assertion helpers outlived their supervisor",
            ));
        }
        let status = self.child.wait()?;
        self.reaped = true;
        let code = match (status.code(), status.signal()) {
            (Some(code), _) => code,
            (_, Some(signal)) => -signal,
            _ => return Err(Error::Policy("unknown native supervisor status")),
        };
        self.code = Some(code);
        Ok(code)
    }
    pub fn abort(&mut self, progress: &Progress) -> Result<(), Error> {
        if self.reaped {
            return Ok(());
        }
        if self.exited()?.is_none() && self.stop_deadline.is_none() {
            self.signal()?;
        }
        let deadline = self.stop_deadline.unwrap_or_else(Instant::now);
        let mut next = Instant::now();
        while self.exited()?.is_none() && Instant::now() < deadline {
            if Instant::now() >= next {
                progress.say("Stopping owned invocation after failure (bounded cleanup)")?;
                next = Instant::now() + Duration::from_secs(15);
            }
            thread::sleep(Duration::from_millis(100));
        }
        let forced = self.exited()?.is_none();
        if forced {
            rustix::process::kill_process(self.pid, Signal::KILL)?;
        }
        self.abort_helpers()?;
        self.finish()?;
        if forced {
            Err(Error::Policy(
                "supervisor cleanup exceeded its deadline; private processes killed, fixture resources unverified",
            ))
        } else {
            Ok(())
        }
    }
    fn abort_helpers(&mut self) -> Result<(), Error> {
        for kind in [Signal::TERM, Signal::KILL] {
            process::signal_session(self.id(), kind)?;
            let deadline = Instant::now() + Duration::from_secs(5);
            while !process::session_members(Path::new("/proc"), self.id())?.is_empty()
                && Instant::now() < deadline
            {
                thread::sleep(Duration::from_millis(50));
            }
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.exited()?.is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }
    fn force_cleanup(&mut self) -> Result<(), Error> {
        if self.reaped {
            return Ok(());
        }
        if self.exited()?.is_none() {
            rustix::process::kill_process(self.pid, Signal::KILL)?;
        }
        self.abort_helpers()?;
        self.finish()?;
        Ok(())
    }
}
impl Drop for Supervisor {
    fn drop(&mut self) {
        if let Err(error) = self.force_cleanup() {
            tracing::error!(%error,"owned native interruption cleanup failed");
        }
    }
}

fn pinned_read(directory: &impl std::os::fd::AsFd, name: &str) -> Result<Vec<u8>, Error> {
    let fd = rustix::fs::openat(
        directory,
        name,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    let mut bytes = Vec::new();
    File::from(fd).take(65536).read_to_end(&mut bytes)?;
    Ok(bytes)
}
fn diagnostics(leader: u32) -> Result<Vec<serde_json::Value>, Error> {
    use std::os::fd::AsRawFd;
    let mut records = Vec::new();
    for pid in process::session_members(Path::new("/proc"), leader)? {
        let directory = match rustix::fs::open(
            format!("/proc/{pid}"),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        ) {
            Ok(directory) => directory,
            Err(rustix::io::Errno::NOENT | rustix::io::Errno::SRCH) => continue,
            Err(error) => return Err(error.into()),
        };
        let mut record = serde_json::json!({"pid":pid});
        let observe = (|| {
            let stat = process::Record::parse(&pinned_read(&directory, "stat")?)?;
            if stat.session != leader {
                return Ok::<_, Error>(false);
            }
            record["state"] = String::from_utf8(vec![stat.state])
                .map_err(|_| Error::Policy("invalid helper state"))?
                .into();
            record["parent"] = stat.parent.into();
            record["group"] = stat.group.into();
            record["session"] = stat.session.into();
            record["start_ticks"] = stat.start_ticks.into();
            for name in ["cmdline", "status", "wchan"] {
                match pinned_read(&directory, name) {
                    Ok(bytes) => {
                        if name == "cmdline" {
                            record[name] = bytes
                                .split(|byte| *byte == 0)
                                .filter(|piece| !piece.is_empty())
                                .map(|piece| String::from_utf8_lossy(piece).into_owned())
                                .collect::<Vec<_>>()
                                .into();
                            record["cmdline_bytes"] = serde_json::to_value(bytes)?;
                        } else {
                            record[name] = String::from_utf8_lossy(&bytes).into_owned().into();
                        }
                    }
                    Err(error) => record[format!("{name}_error")] = error.to_string().into(),
                }
            }
            for name in ["exe", "ns/mnt"] {
                match rustix::fs::readlinkat(&directory, name, Vec::new()) {
                    Ok(value) => record[name] = value.to_string_lossy().into_owned().into(),
                    Err(error) => record[format!("{name}_error")] = error.to_string().into(),
                }
            }
            let tasks = rustix::fs::openat(
                &directory,
                "task",
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )?;
            let mut threads = Vec::new();
            for task in fs::read_dir(PathBuf::from(format!(
                "/proc/self/fd/{}",
                tasks.as_raw_fd()
            )))? {
                let task = task?;
                let name = task.file_name();
                let tid: u32 = name
                    .to_str()
                    .ok_or(Error::Policy("invalid helper thread identity"))?
                    .parse()
                    .map_err(|_| Error::Policy("invalid helper thread identity"))?;
                let thread = match rustix::fs::openat(
                    &tasks,
                    name,
                    rustix::fs::OFlags::RDONLY
                        | rustix::fs::OFlags::DIRECTORY
                        | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::empty(),
                ) {
                    Ok(thread) => thread,
                    Err(rustix::io::Errno::NOENT | rustix::io::Errno::SRCH) => continue,
                    Err(error) => return Err(error.into()),
                };
                let value = match pinned_read(&thread, "stat") {
                    Ok(bytes) => {
                        serde_json::json!({"tid":tid,"stat":String::from_utf8_lossy(&bytes)})
                    }
                    Err(error) => serde_json::json!({"tid":tid,"error":error.to_string()}),
                };
                threads.push(value);
            }
            record["threads"] = threads.into();
            Ok(true)
        })();
        match observe {
            Ok(false) => continue,
            Ok(true) => {}
            Err(error) => record["error"] = error.to_string().into(),
        }
        records.push(record);
    }
    Ok(records)
}
