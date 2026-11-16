//! Bounded independent verifier processes. Results are never reused across jobs.
use rustix::process::{Pid, Signal};
use std::{
    fs::{File, OpenOptions},
    io,
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// parallel verifier I/O failed: {0}
    Io(#[from] io::Error),
    /// invalid parallel verifier arguments
    Arguments,
    /// parallel verification cancelled, orphaned, or exceeded its 595-second deadline
    Cancelled,
}

struct Worker {
    child: Child,
    group: Pid,
    index: usize,
    stopped: bool,
}

impl Drop for Worker {
    fn drop(&mut self) {
        // ESRCH means the owned group has already exited. Other failures remain visible.
        if !self.stopped
            && let Err(error) = signal(self.group, Signal::KILL)
        {
            use std::io::Write;
            let _ = writeln!(io::stderr().lock(), "cannot stop verifier group: {error}");
        }
        let _ = self.child.wait(); // Reap after terminating our owned group, including on early errors.
    }
}

fn signal(group: Pid, signal: Signal) -> io::Result<()> {
    match rustix::process::kill_process_group(group, signal) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn output(directory: &Path, index: usize, suffix: &str) -> PathBuf {
    directory.join(format!("{index}.{suffix}"))
}

fn create(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

fn start(directory: &Path, program: &Path, target: &str, index: usize) -> Result<Worker, Error> {
    let child = Command::new(program)
        .arg(target)
        .process_group(0)
        .stdout(create(&output(directory, index, "stdout"))?)
        .stderr(create(&output(directory, index, "stderr"))?)
        .spawn()?;
    let group = Pid::from_raw(i32::try_from(child.id()).map_err(|_| Error::Arguments)?)
        .ok_or(Error::Arguments)?;
    Ok(Worker {
        child,
        group,
        index,
        stopped: false,
    })
}

pub fn run(
    directory: &Path,
    program: &Path,
    target: &str,
    count: usize,
    parent: u32,
) -> Result<(), Error> {
    if !(2..=256).contains(&count) || parent == 0 || target.is_empty() {
        return Err(Error::Arguments);
    }
    let deadline = Instant::now() + Duration::from_secs(595);
    let mut active: Vec<Worker> = Vec::new();
    let mut next = 0;
    loop {
        if Instant::now() >= deadline
            || directory.join("cancel").try_exists()?
            || u32::try_from(Pid::as_raw(rustix::process::getppid())).ok() != Some(parent)
        {
            for worker in &active {
                signal(worker.group, Signal::TERM)?;
            }
            thread::sleep(Duration::from_millis(100));
            return Err(Error::Cancelled);
        }
        while next < count && active.len() < 4 {
            active.push(start(directory, program, target, next)?);
            next += 1;
        }
        let mut index = 0;
        while index < active.len() {
            if let Some(status) = active[index].child.try_wait()? {
                use std::{io::Write, os::unix::process::ExitStatusExt};
                let mut worker = active.remove(index);
                let code = status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(1));
                let number = worker.index;
                signal(worker.group, Signal::KILL)?;
                worker.stopped = true;
                drop(worker); // No surviving ordinary descendants when a result is published.
                write!(create(&output(directory, number, "status"))?, "{code}")?;
            } else {
                index += 1;
            }
        }
        if next == count && active.is_empty() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
}
