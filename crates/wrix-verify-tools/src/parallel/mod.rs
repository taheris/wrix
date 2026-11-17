//! Bounded independent verifier processes. Results are never reused across jobs.
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions};
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

impl Worker {
    fn exited(&self) -> io::Result<bool> {
        loop {
            match rustix::process::waitid(
                WaitId::Pid(self.group),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            ) {
                Ok(status) => return Ok(status.is_some()),
                Err(rustix::io::Errno::INTR) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn signal(&self, signal: Signal) -> io::Result<()> {
        match rustix::process::kill_process_group(self.group, signal) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
            #[cfg(target_os = "macos")]
            Err(rustix::io::Errno::PERM) if self.exited()? && zombie_group(self.group)? => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn finish(&mut self) -> io::Result<std::process::ExitStatus> {
        self.signal(Signal::KILL)?;
        self.stopped = true;
        self.child.wait()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if !self.stopped
            && let Err(error) = self.signal(Signal::KILL)
        {
            report_cleanup(&error);
        }
        if let Err(error) = self.child.wait() {
            report_cleanup(&error);
        }
    }
}

fn report_cleanup(error: &io::Error) {
    use std::io::Write;
    if writeln!(
        io::stderr().lock(),
        "cannot clean up verifier group: {error}"
    )
    .is_err()
    {
        std::process::exit(74);
    }
}

#[cfg(target_os = "macos")]
fn zombie_group(group: Pid) -> io::Result<bool> {
    let output = Command::new(option_env!("WRIX_PARALLEL_PS").unwrap_or("ps"))
        .args([
            "-g",
            &group.as_raw_nonzero().get().to_string(),
            "-o",
            "stat=",
        ])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "verifier group observation failed: {output:?}"
        )));
    }
    Ok(only_zombies(&output.stdout))
}

#[cfg(any(test, target_os = "macos"))]
fn only_zombies(states: &[u8]) -> bool {
    let mut members = states
        .split(u8::is_ascii_whitespace)
        .filter(|member| !member.is_empty())
        .peekable();
    members.peek().is_some() && members.all(|member| member.first() == Some(&b'Z'))
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
                worker.signal(Signal::TERM)?;
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
            if active[index].exited()? {
                use std::{io::Write, os::unix::process::ExitStatusExt};
                let mut worker = active.remove(index);
                let status = worker.finish()?;
                let code = status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(1));
                let number = worker.index;
                drop(worker); // Descendant cleanup precedes reaping and result publication.
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

#[cfg(test)]
mod test;
