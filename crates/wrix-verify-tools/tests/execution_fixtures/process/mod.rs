use rustix::process::{Pid, Signal, WaitId, WaitIdOptions};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::{fs::FileExt, process::CommandExt},
    path::Path,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

pub struct Owned {
    child: Option<Child>,
    pid: Pid,
    stdout: File,
    stderr: File,
}

impl Owned {
    pub fn spawn(command: &mut Command) -> Self {
        let stdout = tempfile::tempfile().unwrap();
        let stderr = tempfile::tempfile().unwrap();
        let child = command
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(stdout.try_clone().unwrap())
            .stderr(stderr.try_clone().unwrap())
            .spawn()
            .unwrap();
        let pid = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
        Self {
            child: Some(child),
            pid,
            stdout,
            stderr,
        }
    }

    fn exited(&self) -> bool {
        rustix::process::waitid(
            WaitId::Pid(self.pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )
        .unwrap()
        .is_some()
    }

    pub fn id(&self) -> u32 {
        self.child.as_ref().unwrap().id()
    }

    pub fn close_stdin(&mut self) {
        drop(self.child.as_mut().unwrap().stdin.take());
    }

    fn diagnostics(&self) -> String {
        let mut captured = Vec::new();
        for file in [&self.stdout, &self.stderr] {
            let mut buffer = vec![0; 1_048_576];
            let size = file.read_at(&mut buffer, 0).unwrap();
            captured.extend_from_slice(&buffer[..size]);
        }
        String::from_utf8_lossy(&captured).into_owned()
    }

    pub fn ready(&self, marker: &Path) {
        let deadline = Instant::now() + Duration::from_secs(120);
        while !marker.exists() {
            assert!(
                !self.exited(),
                "process exited before {}: {}",
                marker.display(),
                self.diagnostics()
            );
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {}: {}",
                marker.display(),
                self.diagnostics()
            );
            thread::sleep(Duration::from_millis(50));
        }
        assert!(!self.exited(), "fixture did not wait for release");
    }

    #[cfg(target_os = "macos")]
    fn group_has_only_zombies(&self) -> std::io::Result<bool> {
        let output = Command::new("ps")
            .args([
                "-g",
                &self.pid.as_raw_nonzero().get().to_string(),
                "-o",
                "stat=",
            ])
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "owned group observation failed: {output:?}"
            )));
        }
        Ok(!output.stdout.is_empty()
            && output
                .stdout
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .all(|line| {
                    line.iter()
                        .copied()
                        .find(|byte| !byte.is_ascii_whitespace())
                        == Some(b'Z')
                }))
    }

    pub fn stop(&mut self) -> std::io::Result<std::process::ExitStatus> {
        // Keep the leader waitable until signaling, so its group ID cannot be recycled.
        match rustix::process::kill_process_group(self.pid, Signal::KILL) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => {}
            #[cfg(target_os = "macos")]
            Err(rustix::io::Errno::PERM) if self.exited() && self.group_has_only_zombies()? => {
                // Darwin refuses to signal zombie-only groups; observe this exact held group.
            }
            Err(error) => return Err(error.into()),
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        while !self.exited() {
            assert!(
                Instant::now() < deadline,
                "execution cleanup exceeded 30 seconds"
            );
            thread::sleep(Duration::from_millis(10));
        }
        self.child.take().unwrap().wait()
    }

    pub fn finish(self, input: &[u8]) -> Output {
        self.finish_with_limit(input, Duration::from_secs(120))
    }

    pub fn finish_with_limit(mut self, input: &[u8], limit: Duration) -> Output {
        if let Some(mut stdin) = self.child.as_mut().unwrap().stdin.take() {
            stdin.write_all(input).unwrap();
        } else {
            assert_eq!(input, b"", "cannot send input to a closed execution stream");
        }
        let deadline = Instant::now() + limit;
        while !self.exited() {
            assert!(Instant::now() < deadline, "execution exceeded {limit:?}");
            thread::sleep(Duration::from_millis(10));
        }
        let status = self.stop().unwrap();
        self.stdout.seek(SeekFrom::Start(0)).unwrap();
        self.stderr.seek(SeekFrom::Start(0)).unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        self.stdout.read_to_end(&mut stdout).unwrap();
        self.stderr.read_to_end(&mut stderr).unwrap();
        Output {
            status,
            stdout,
            stderr,
        }
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        if self.child.is_some()
            && let Err(error) = self.stop()
        {
            eprintln!("owned execution cleanup failed: {error}");
        }
    }
}

#[test]
fn dropping_an_unreleased_fixture_terminates_and_reaps_its_leader() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("ready");
    let mut child = Owned::spawn(
        Command::new("bash")
            .args(["-c", "touch \"$1\"; exec sleep 30", "fixture"])
            .arg(&marker),
    );
    child.close_stdin();
    child.ready(&marker);
    let pid = child.pid;
    drop(child);
    assert!(matches!(
        rustix::process::waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG,
        ),
        Err(rustix::io::Errno::CHILD)
    ));
}

pub fn run(command: &mut Command, input: &[u8]) -> Output {
    Owned::spawn(command).finish(input)
}
