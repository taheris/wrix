pub mod mapped;
use super::{Bootstrap, Change, Delegation, Events};
use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::{self, Write},
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt},
        process::ExitStatusExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// cgroup probe filesystem or process failed: {0}
    Io(#[from] io::Error),
    /// cgroup probe descriptor failed: {0}
    Descriptor(#[from] rustix::io::Errno),
    /// cgroup probe observation failed: {0}
    Observation(#[from] super::Error),
    /// cgroup probe requires a private mount namespace and cgroup-v2 root
    Root,
    /// cgroup child did not terminate within its original deadline
    Timeout,
    /// cgroup resource isolation assertion failed
    Evidence,
    /// cgroup cleanup did not restore all owned resources: {0:?}
    Cleanup(Vec<String>),
    /// cgroup probe failed ({primary}) and cleanup also failed: {cleanup}
    Combined {
        primary: Box<Self>,
        cleanup: Box<Self>,
    },
    /// invalid internal owned child handle
    Child,
}

#[derive(Clone, Copy)]
enum Role {
    Core,
    Builder,
}
impl Role {
    const fn name(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Builder => "builder",
        }
    }
}
#[derive(Clone, Copy)]
enum Budget {
    Direct,
    Mapped,
}
impl Budget {
    const fn builder(self) -> (&'static [u8], &'static [u8]) {
        match self {
            Self::Direct => (b"67108864", b"8"),
            Self::Mapped => (b"134217728", b"32"),
        }
    }
}
#[derive(Clone, Copy)]
struct Handle(usize);
enum Undo {
    Admin,
    Delegation(Delegation),
    Parent,
    Role(Role),
    Child(Child),
}
struct Session {
    root: PathBuf,
    undo: Vec<Undo>,
}
impl Session {
    fn admit(root: &Path) -> Result<(Self, Delegation), Error> {
        if !root.is_absolute()
            || fs::canonicalize(root)? != root
            || !rustix::process::geteuid().is_root()
            || rustix::fs::statfs(root)?.f_type != 0x6367_7270
            || fs::read_link("/proc/self/ns/mnt")? == fs::read_link("/proc/1/ns/mnt")?
        {
            return Err(Error::Root);
        }
        let plan = Delegation::parse(
            &fs::read(root.join("cgroup.controllers"))?,
            &fs::read(root.join("cgroup.subtree_control"))?,
        )?;
        let session = Self {
            root: root.to_owned(),
            undo: Vec::new(),
        };
        for name in ["wrix_probe_admin", "wrix_probe_roles"] {
            match fs::symlink_metadata(root.join(name)) {
                Ok(_) => return Err(Error::Root),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Self::bootstrap(&root.join("cgroup.procs"))?;
        Ok((session, plan))
    }
    fn parent(&self) -> PathBuf {
        self.root.join("wrix_probe_roles")
    }
    fn role(&self, role: Role) -> PathBuf {
        self.parent().join(role.name())
    }
    fn bootstrap(file: &Path) -> Result<Vec<Bootstrap>, Error> {
        let pids = fs::read_to_string(file)?;
        pids.split_whitespace()
            .map(|pid| {
                Ok(Bootstrap::parse(
                    pid,
                    &fs::read(Path::new("/proc").join(pid).join("status"))?,
                )?)
            })
            .collect()
    }
    fn mkdir(path: &Path) -> Result<(), Error> {
        DirBuilder::new().mode(0o700).create(path)?;
        Ok(())
    }
    fn write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
        OpenOptions::new()
            .write(true)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
            .open(path)?
            .write_all(bytes)?;
        Ok(())
    }
    fn prepare(&mut self, plan: Delegation, budget: Budget) -> Result<(), Error> {
        let admin = self.root.join("wrix_probe_admin");
        Self::mkdir(&admin)?;
        self.undo.push(Undo::Admin);
        for pid in Self::bootstrap(&self.root.join("cgroup.procs"))? {
            Self::write(
                &admin.join("cgroup.procs"),
                pid.raw().to_string().as_bytes(),
            )?;
        }
        if !fs::read_to_string(self.root.join("cgroup.procs"))?
            .trim()
            .is_empty()
        {
            return Err(Error::Evidence);
        }
        let enable = plan.command(Change::Enable);
        self.undo.push(Undo::Delegation(plan));
        if !enable.is_empty() {
            Self::write(&self.root.join("cgroup.subtree_control"), enable.as_bytes())?;
        }
        DirBuilder::new().mode(0o755).create(self.parent())?;
        self.undo.push(Undo::Parent);
        Self::write(
            &self.parent().join("cgroup.subtree_control"),
            b"+cpu +memory +pids",
        )?;
        let (builder_memory, builder_pids) = budget.builder();
        for (role, memory, pids, cpu) in [
            (
                Role::Core,
                b"134217728".as_slice(),
                b"16".as_slice(),
                b"100000 100000".as_slice(),
            ),
            (Role::Builder, builder_memory, builder_pids, b"50000 100000"),
        ] {
            Self::mkdir(&self.role(role))?;
            self.undo.push(Undo::Role(role));
            for (file, value) in [("memory.max", memory), ("pids.max", pids), ("cpu.max", cpu)] {
                Self::write(&self.role(role).join(file), value)?;
            }
        }
        Ok(())
    }
    fn spawn(&mut self, role: Role) -> Result<Handle, Error> {
        let operation = match role {
            Role::Core => "idle",
            Role::Builder => "root-memory",
        };
        let child = Command::new(std::env::current_exe()?)
            .env_clear()
            .args(["operator-probe", "worker", operation])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let pid = child.id();
        let handle = Handle(self.undo.len());
        self.undo.push(Undo::Child(child));
        Self::write(
            &self.role(role).join("cgroup.procs"),
            pid.to_string().as_bytes(),
        )?;
        self.child(handle)?
            .stdin
            .take()
            .ok_or(Error::Child)?
            .write_all(b"1")?;
        Ok(handle)
    }
    fn child(&mut self, handle: Handle) -> Result<&mut Child, Error> {
        match self.undo.get_mut(handle.0) {
            Some(Undo::Child(child)) => Ok(child),
            _ => Err(Error::Child),
        }
    }
    fn wait(child: &mut Child, limit: Duration) -> Result<ExitStatus, Error> {
        let until = Instant::now() + limit;
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= until {
                return Err(Error::Timeout);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn observe(&mut self) -> Result<Receipt, Error> {
        let core = self.spawn(Role::Core)?;
        let worker = self.spawn(Role::Builder)?;
        if Self::wait(self.child(worker)?, Duration::from_secs(20))?.signal() != Some(9)
            || self.child(core)?.try_wait()?.is_some()
        {
            return Err(Error::Evidence);
        }
        let events = Events::parse(&fs::read(self.role(Role::Builder).join("memory.events"))?)?;
        if events.oom_kill == 0
            || Events::parse(&fs::read(self.role(Role::Core).join("memory.events"))?)?.oom_kill != 0
            || fs::read_to_string(self.role(Role::Builder).join("cpu.max"))?.trim()
                != "50000 100000"
            || fs::read_to_string(self.role(Role::Builder).join("pids.max"))?.trim() != "8"
        {
            return Err(Error::Evidence);
        }
        Ok(Receipt {
            scope: Scope::Primitive,
            builder_memory_max: 67_108_864,
            builder_oom_kill: events.oom_kill,
            core_survived: true,
            builder_cpu_max: "50000 100000",
            builder_pids_max: 8,
        })
    }
    fn stop(child: &mut Child) -> Result<(), Error> {
        if child.try_wait()?.is_none() {
            let pid = rustix::process::Pid::from_raw(
                i32::try_from(child.id()).map_err(|_| Error::Child)?,
            )
            .ok_or(Error::Child)?;
            match rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()) {
                Ok(fd) => {
                    match rustix::process::pidfd_send_signal(&fd, rustix::process::Signal::TERM) {
                        Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                Err(rustix::io::Errno::SRCH) => {}
                Err(error) => return Err(error.into()),
            }
            Self::wait(child, Duration::from_secs(10))?;
        }
        Ok(())
    }
    fn cleanup(&mut self) -> Result<(), Error> {
        let mut errors = Vec::new();
        while let Some(action) = self.undo.pop() {
            let result = match action {
                Undo::Child(mut child) => Self::stop(&mut child),
                Undo::Role(role) => fs::remove_dir(self.role(role)).map_err(Error::from),
                Undo::Parent => fs::remove_dir(self.parent()).map_err(Error::from),
                Undo::Delegation(plan) => (|| {
                    let command = plan.command(Change::Restore);
                    if !command.is_empty() {
                        Self::write(
                            &self.root.join("cgroup.subtree_control"),
                            command.as_bytes(),
                        )?;
                    }
                    if !plan.restored(&fs::read(self.root.join("cgroup.subtree_control"))?)? {
                        return Err(Error::Evidence);
                    }
                    Ok(())
                })(),
                Undo::Admin => (|| {
                    let admin = self.root.join("wrix_probe_admin");
                    for pid in Self::bootstrap(&admin.join("cgroup.procs"))? {
                        Self::write(
                            &self.root.join("cgroup.procs"),
                            pid.raw().to_string().as_bytes(),
                        )?;
                    }
                    fs::remove_dir(admin)?;
                    Ok(())
                })(),
            };
            if let Err(error) = result {
                errors.push(error.to_string());
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(Error::Cleanup(errors))
        }
    }
}
#[derive(serde::Serialize)]
enum Scope {
    #[serde(
        rename = "native cgroup v2 resource primitive, not full service/Nix integration acceptance"
    )]
    Primitive,
}
#[derive(serde::Serialize)]
pub struct Receipt {
    scope: Scope,
    builder_memory_max: u64,
    builder_oom_kill: u64,
    core_survived: bool,
    builder_cpu_max: &'static str,
    builder_pids_max: u64,
}
pub fn run(root: &Path) -> Result<Receipt, Error> {
    let (mut session, plan) = Session::admit(root)?;
    let result = session
        .prepare(plan, Budget::Direct)
        .and_then(|()| session.observe());
    match (result, session.cleanup()) {
        (Ok(receipt), Ok(())) => Ok(receipt),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(Error::Combined {
            primary: Box::new(primary),
            cleanup: Box::new(cleanup),
        }),
    }
}
#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn ordinary_filesystem_cannot_be_mutated_as_a_cgroup_probe() {
        let temp = tempfile::tempdir().unwrap();
        let path = fs::canonicalize(temp.path()).unwrap();
        assert!(matches!(Session::admit(&path), Err(Error::Root)));
        assert_eq!(fs::read_dir(path).unwrap().count(), 0);
    }
}
