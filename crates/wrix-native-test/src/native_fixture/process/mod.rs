#[cfg(target_os = "linux")]
use rustix::process::{Pid, Signal};
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::{fs, io, path::Path};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// native process observation failed: {0}
    Io(#[from] io::Error),
    /// malformed native process record
    Record,
    /// native process signal failed: {0}
    Signal(#[from] rustix::io::Errno),
}

#[derive(Debug, Eq, PartialEq)]
pub struct Record {
    pub state: u8,
    pub parent: u32,
    pub group: u32,
    pub session: u32,
    pub start_ticks: u64,
}
impl Record {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let end = bytes
            .iter()
            .rposition(|byte| *byte == b')')
            .ok_or(Error::Record)?;
        let fields: Vec<_> = bytes[end + 1..]
            .split(u8::is_ascii_whitespace)
            .filter(|field| !field.is_empty())
            .collect();
        let number = |index: usize| -> Result<u64, Error> {
            std::str::from_utf8(fields.get(index).ok_or(Error::Record)?)
                .map_err(|_| Error::Record)?
                .parse()
                .map_err(|_| Error::Record)
        };
        Ok(Self {
            state: *fields
                .first()
                .filter(|field| field.len() == 1)
                .and_then(|field| field.first())
                .ok_or(Error::Record)?,
            parent: u32::try_from(number(1)?).map_err(|_| Error::Record)?,
            group: u32::try_from(number(2)?).map_err(|_| Error::Record)?,
            session: u32::try_from(number(3)?).map_err(|_| Error::Record)?,
            start_ticks: number(19)?,
        })
    }
}
fn gone(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::NotFound)
        || error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error())
}
fn read(path: &Path) -> Result<Option<Record>, Error> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(Record::parse(&bytes)?)),
        Err(error) if gone(&error) => Ok(None),
        Err(error) => Err(error.into()),
    }
}
pub fn session_members(proc: &Path, leader: u32) -> Result<Vec<u32>, Error> {
    let mut members = Vec::new();
    for entry in fs::read_dir(proc)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.as_encoded_bytes().is_empty()
            || !name.as_encoded_bytes().iter().all(u8::is_ascii_digit)
        {
            continue;
        }
        let pid: u32 = name
            .to_str()
            .ok_or(Error::Record)?
            .parse()
            .map_err(|_| Error::Record)?;
        if pid == leader {
            continue;
        }
        let Some(record) = read(&entry.path().join("stat"))? else {
            continue;
        };
        if record.session != leader {
            continue;
        }
        let mut active = record.state != b'Z';
        if !active {
            let tasks = match fs::read_dir(entry.path().join("task")) {
                Ok(tasks) => tasks,
                Err(error) if gone(&error) => continue,
                Err(error) => return Err(error.into()),
            };
            for task in tasks {
                if let Some(record) = read(&task?.path().join("stat"))? {
                    active |= record.state != b'Z';
                }
            }
        }
        if active {
            members.push(pid);
        }
    }
    members.sort_unstable();
    Ok(members)
}

#[cfg(target_os = "linux")]
pub fn signal_session(leader: u32, kind: Signal) -> Result<Vec<u32>, Error> {
    let proc = Path::new("/proc");
    let mut signalled = Vec::new();
    for pid in session_members(proc, leader)? {
        let native =
            Pid::from_raw(i32::try_from(pid).map_err(|_| Error::Record)?).ok_or(Error::Record)?;
        let descriptor =
            match rustix::process::pidfd_open(native, rustix::process::PidfdFlags::empty()) {
                Ok(descriptor) => descriptor,
                Err(rustix::io::Errno::SRCH) => continue,
                Err(error) => return Err(error.into()),
            };
        if session_members(proc, leader)?.contains(&pid) {
            match rustix::process::pidfd_send_signal(&descriptor, kind) {
                Ok(()) => signalled.push(pid),
                Err(rustix::io::Errno::SRCH) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(signalled)
}

#[cfg(any(target_os = "linux", test))]
trait ChildObservation {
    type Handle;
    fn caller_parent(&mut self) -> Option<u32>;
    fn pin(&mut self, pid: u32) -> Result<Option<Self::Handle>, Error>;
    fn parent(&mut self, pid: u32) -> Result<Option<u32>, Error>;
    fn signal(&mut self, handle: &Self::Handle) -> Result<bool, Error>;
}
#[cfg(any(target_os = "linux", test))]
fn signal_child_with(
    pid: u32,
    parent: u32,
    observer: &mut impl ChildObservation,
) -> Result<bool, Error> {
    if pid == 0
        || pid == std::process::id()
        || parent == 0
        || observer.caller_parent() != Some(parent)
    {
        return Ok(false);
    }
    let Some(handle) = observer.pin(pid)? else {
        return Ok(false);
    };
    if observer.parent(pid)? != Some(parent) || observer.caller_parent() != Some(parent) {
        return Ok(false);
    }
    observer.signal(&handle)
}
#[cfg(target_os = "linux")]
pub fn signal_child(pid: u32, parent: u32) -> Result<bool, Error> {
    struct Native;
    impl ChildObservation for Native {
        type Handle = std::os::fd::OwnedFd;
        fn caller_parent(&mut self) -> Option<u32> {
            rustix::process::getppid().map(|id| id.as_raw_nonzero().get().cast_unsigned())
        }
        fn pin(&mut self, pid: u32) -> Result<Option<Self::Handle>, Error> {
            let native = Pid::from_raw(i32::try_from(pid).map_err(|_| Error::Record)?)
                .ok_or(Error::Record)?;
            match rustix::process::pidfd_open(native, rustix::process::PidfdFlags::empty()) {
                Ok(fd) => Ok(Some(fd)),
                Err(rustix::io::Errno::SRCH) => Ok(None),
                Err(error) => Err(error.into()),
            }
        }
        fn parent(&mut self, pid: u32) -> Result<Option<u32>, Error> {
            Ok(read(&PathBuf::from(format!("/proc/{pid}/stat")))?.map(|record| record.parent))
        }
        fn signal(&mut self, handle: &Self::Handle) -> Result<bool, Error> {
            match rustix::process::pidfd_send_signal(handle, Signal::TERM) {
                Ok(()) => Ok(true),
                Err(rustix::io::Errno::SRCH) => Ok(false),
                Err(error) => Err(error.into()),
            }
        }
    }
    signal_child_with(pid, parent, &mut Native)
}

#[cfg(test)]
mod test {
    use super::*;

    struct Observation {
        parents: std::collections::VecDeque<u32>,
        child_parent: u32,
        sent: bool,
    }
    impl ChildObservation for Observation {
        type Handle = ();
        fn caller_parent(&mut self) -> Option<u32> {
            self.parents.pop_front()
        }
        fn pin(&mut self, _pid: u32) -> Result<Option<()>, Error> {
            Ok(Some(()))
        }
        fn parent(&mut self, _pid: u32) -> Result<Option<u32>, Error> {
            Ok(Some(self.child_parent))
        }
        fn signal(&mut self, _handle: &()) -> Result<bool, Error> {
            self.sent = true;
            Ok(true)
        }
    }
    #[test]
    fn reused_pid_with_foreign_parent_cannot_receive_the_original_signal() {
        let mut observation = Observation {
            parents: [111, 111].into(),
            child_parent: 999,
            sent: false,
        };
        assert!(!signal_child_with(222, 111, &mut observation).unwrap());
        assert!(!observation.sent);
    }
    #[test]
    fn parent_loss_after_pidfd_acquisition_revokes_signal_authority() {
        let mut observation = Observation {
            parents: [111, 1].into(),
            child_parent: 111,
            sent: false,
        };
        assert!(!signal_child_with(222, 111, &mut observation).unwrap());
        assert!(!observation.sent);
    }
    #[test]
    fn unchanged_direct_child_and_live_parent_allow_only_the_pinned_signal() {
        let mut observation = Observation {
            parents: [111, 111].into(),
            child_parent: 111,
            sent: false,
        };
        assert!(signal_child_with(222, 111, &mut observation).unwrap());
        assert!(observation.sent);
    }
    fn stat(state: char, session: u32) -> String {
        format!(
            "222 (owned ) worker) {state} 1 222 {session} {} 77",
            std::iter::repeat_n("0", 15).collect::<Vec<_>>().join(" ")
        )
    }
    #[test]
    fn stat_handles_parentheses_in_names_and_preserves_birth_identity() {
        assert_eq!(
            Record::parse(stat('S', 111).as_bytes()).unwrap(),
            Record {
                state: b'S',
                parent: 1,
                group: 222,
                session: 111,
                start_ticks: 77
            }
        );
        assert!(Record::parse(b"short").is_err());
        assert!(Record::parse(b"1 (name) garbage").is_err());
    }
    #[test]
    fn zombie_leader_does_not_hide_live_threads_in_its_session() {
        let proc = tempfile::tempdir().unwrap();
        let path = proc.path().join("222");
        fs::create_dir_all(path.join("task/222")).unwrap();
        fs::create_dir_all(path.join("task/223")).unwrap();
        fs::write(path.join("stat"), stat('Z', 111)).unwrap();
        fs::write(path.join("task/222/stat"), stat('Z', 111)).unwrap();
        fs::write(path.join("task/223/stat"), stat('S', 111)).unwrap();
        assert_eq!(session_members(proc.path(), 111).unwrap(), vec![222]);
        fs::write(path.join("task/223/stat"), stat('Z', 111)).unwrap();
        assert_eq!(session_members(proc.path(), 111).unwrap(), [] as [u32; 0]);
    }
    #[test]
    fn unrelated_session_and_unreaped_leader_are_not_helpers() {
        let proc = tempfile::tempdir().unwrap();
        for (pid, session) in [(111, 111), (222, 999)] {
            let path = proc.path().join(pid.to_string());
            fs::create_dir(&path).unwrap();
            fs::write(path.join("stat"), stat('S', session)).unwrap();
        }
        assert_eq!(session_members(proc.path(), 111).unwrap(), [] as [u32; 0]);
    }
}
