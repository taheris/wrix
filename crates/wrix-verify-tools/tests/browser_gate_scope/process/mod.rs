use rustix::process::{Pid, Signal, WaitId, WaitIdOptions};
use std::{
    fs,
    io::{self, Read, Seek, SeekFrom},
    os::unix::process::CommandExt,
    process::{Child, Command, ExitStatus, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

pub struct Owned {
    child: Option<Child>,
    pid: Pid,
}
impl Owned {
    pub fn spawn(command: &mut Command) -> Self {
        let child = command.process_group(0).spawn().unwrap();
        let pid = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
        Self {
            child: Some(child),
            pid,
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
    fn signal(&self, signal: Signal) {
        match rustix::process::kill_process_group(self.pid, signal) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => {}
            Err(error) => panic!("owned fixture group signal failed: {error}"),
        }
    }
    fn active(&self) -> bool {
        for entry in fs::read_dir("/proc").unwrap() {
            let entry = entry.unwrap();
            if !entry
                .file_name()
                .as_encoded_bytes()
                .iter()
                .all(u8::is_ascii_digit)
            {
                continue;
            }
            let bytes = match fs::read(entry.path().join("stat")) {
                Ok(bytes) => bytes,
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        || error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()) =>
                {
                    continue;
                }
                Err(error) => panic!("fixture process observation failed: {error}"),
            };
            let end = bytes.iter().rposition(|byte| *byte == b')').unwrap();
            let fields: Vec<_> = bytes[end + 1..]
                .split(u8::is_ascii_whitespace)
                .filter(|field| !field.is_empty())
                .collect();
            let group: i32 = std::str::from_utf8(fields[2]).unwrap().parse().unwrap();
            if group == self.pid.as_raw_nonzero().get() && fields[0] != b"Z" {
                return true;
            }
        }
        false
    }
    pub fn stop(&mut self) -> ExitStatus {
        let mut child = self.child.take().unwrap();
        self.signal(Signal::TERM);
        let until = Instant::now() + Duration::from_secs(5);
        while self.active() && Instant::now() < until {
            thread::sleep(Duration::from_millis(10));
        }
        if self.active() {
            self.signal(Signal::KILL);
        }
        let until = Instant::now() + Duration::from_secs(5);
        while self.active() && Instant::now() < until {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!self.active(), "owned fixture group did not stop");
        child.wait().unwrap()
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        if self.child.is_some() {
            self.stop();
        }
    }
}
pub fn bounded(command: &mut Command) -> Output {
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();
    let mut process = Owned::spawn(
        command
            .stdout(Stdio::from(stdout.try_clone().unwrap()))
            .stderr(Stdio::from(stderr.try_clone().unwrap())),
    );
    let until = Instant::now() + Duration::from_secs(180);
    while !process.exited() && Instant::now() < until {
        thread::sleep(Duration::from_millis(10));
    }
    let timed_out = !process.exited();
    let status = process.stop();
    stdout.seek(SeekFrom::Start(0)).unwrap();
    stderr.seek(SeekFrom::Start(0)).unwrap();
    let mut out = Vec::new();
    let mut err = Vec::new();
    stdout.read_to_end(&mut out).unwrap();
    stderr.read_to_end(&mut err).unwrap();
    assert!(
        !timed_out,
        "fixture exceeded180s: {}{}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    Output {
        status,
        stdout: out,
        stderr: err,
    }
}
#[test]
fn owned_runtime_cleanup_terminates_a_real_fixture_child_with_term() {
    use std::os::unix::process::ExitStatusExt;
    let mut unrelated = Owned::spawn(Command::new("sleep").arg("30"));
    let mut child = Owned::spawn(Command::new("sleep").arg("30"));
    assert_eq!(child.stop().signal(), Some(15));
    assert!(!unrelated.exited());
    assert_eq!(unrelated.stop().signal(), Some(15));
}
#[test]
fn normal_exit_keeps_its_status_and_cleans_same_group_background_children() {
    let root = tempfile::tempdir().unwrap();
    let pidfile = root.path().join("pid");
    let result = bounded(Command::new("bash").args(["-c", "sleep 30 & printf '%s' \"$!\" > \"$1\"; printf output; printf diagnostic >&2; exit 37", "fixture"]).arg(&pidfile));
    assert_eq!(result.status.code(), Some(37));
    assert_eq!(result.stdout, b"output");
    assert_eq!(result.stderr, b"diagnostic");
    let pid = fs::read_to_string(pidfile).unwrap();
    match fs::read(format!("/proc/{pid}/stat")) {
        Ok(bytes) => {
            let end = bytes.iter().rposition(|b| *b == b')').unwrap();
            assert_eq!(bytes[end + 2], b'Z');
        }
        Err(error) => assert_eq!(error.kind(), io::ErrorKind::NotFound),
    }
}
