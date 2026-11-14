use super::{Budget, Role, Session};
use crate::native_fixture::command;
use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        process::{CommandExt, ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// invalid mapped cgroup worker arguments or tool package
    Usage,
    /// mapped cgroup probe failed: {0}
    Parent(#[from] super::Error),
    /// mapped cgroup worker I/O failed: {0}
    Io(#[from] io::Error),
    /// mapped cgroup worker descriptor failed: {0}
    Descriptor(#[from] rustix::io::Errno),
    /// mapped cgroup worker command failed: {0}
    Command(#[from] command::Error),
    /// mapped cgroup delegation or resource assertion failed
    Evidence,
    /// mapped namespace actor failed: {status}
    Actor { status: std::process::ExitStatus },
    /// mapped bootstrap did not enter its private root PID/mount namespace
    Namespace,
    /// mapped delegate did not obtain the exact root user mapping
    Mapping,
    /// ancestor budget control owner was not outside the delegated user map
    BudgetOwner,
    /// mapped actor failed ({primary}); bounded self diagnostics also failed: {diagnostic}
    Diagnostic {
        primary: Box<Self>,
        diagnostic: Box<Self>,
    },
    /// mapped cgroup failed ({primary}) and cleanup also failed: {cleanup}
    Combined {
        primary: Box<Self>,
        cleanup: Box<super::Error>,
    },
    /// mapped worker could not restore all owned resources ({primary:?}): {issues:?}
    Cleanup {
        primary: Option<Box<Self>>,
        issues: Vec<String>,
    },
}
struct Package(PathBuf);
#[derive(Clone, Copy)]
enum Tool {
    Mount,
    Unshare,
    Setpriv,
    Umount,
}
impl Package {
    fn parse(path: &Path) -> Result<Self, Error> {
        if path.parent() != Some(Path::new("/nix/store"))
            || fs::canonicalize(path)? != path
            || !path.is_dir()
        {
            return Err(Error::Usage);
        }
        Ok(Self(path.to_owned()))
    }
    fn binary(&self, tool: Tool) -> PathBuf {
        self.0.join("bin").join(match tool {
            Tool::Mount => "mount",
            Tool::Unshare => "unshare",
            Tool::Setpriv => "setpriv",
            Tool::Umount => "umount",
        })
    }
    fn call(&self, tool: Tool, args: &[&std::ffi::OsStr]) -> Result<(), Error> {
        let mut command = Command::new(self.binary(tool));
        command.args(args);
        command::checked(&mut command, Duration::from_secs(10))?;
        Ok(())
    }
}
#[derive(serde::Serialize)]
enum Scope {
    #[serde(
        rename = "native mapped-root cgroup delegation primitive, not full Nix/supervisor integration"
    )]
    Primitive,
}
#[derive(serde::Serialize)]
pub struct Receipt {
    scope: Scope,
    daemon_global_uid: u32,
    ancestor_budget_immutable: bool,
    job_oom_contained: bool,
    core_survived: bool,
}
fn observe(session: &mut Session, util: &Package) -> Result<Receipt, Error> {
    let budget = session.role(Role::Builder);
    for path in [
        budget.clone(),
        budget.join("cgroup.procs"),
        budget.join("cgroup.threads"),
        budget.join("cgroup.subtree_control"),
    ] {
        rustix::fs::chown(
            &path,
            Some(rustix::process::Uid::from_raw(100_000)),
            Some(rustix::process::Gid::from_raw(100_000)),
        )?;
    }
    let core = session.spawn(Role::Core)?;
    let view = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .prefix("wrix-mapped-cgroup-")
        .tempdir()?;
    let path = fs::canonicalize(view.path())?;
    rustix::fs::chown(
        &path,
        Some(rustix::process::Uid::from_raw(100_000)),
        Some(rustix::process::Gid::from_raw(100_000)),
    )?;
    let output = command::capture(
        Command::new(util.binary(Tool::Unshare))
            .env_clear()
            .args([
                "--pid",
                "--fork",
                "--kill-child=TERM",
                "--mount",
                "--propagation",
                "private",
            ])
            .arg(std::env::current_exe()?)
            .args(["operator-probe", "mapped-bootstrap"])
            .arg(&budget)
            .arg(&path)
            .arg(&util.0),
        Duration::from_secs(40),
    )?;
    if !output.status.success() {
        io::stderr().lock().write_all(&output.stderr)?;
        io::stderr().lock().write_all(&output.stdout)?;
        return Err(Error::Actor {
            status: output.status,
        });
    }
    if !output
        .stdout
        .windows(b"MAPPED_DAEMON_CANNOT_RELAX_BUDGET_AND_JOB_OOM_CONTAINED".len())
        .any(|value| value == b"MAPPED_DAEMON_CANNOT_RELAX_BUDGET_AND_JOB_OOM_CONTAINED")
        || session.child(core)?.try_wait()?.is_some()
    {
        return Err(Error::Evidence);
    }
    for (file, expected) in [
        ("memory.max", "134217728"),
        ("pids.max", "32"),
        ("cpu.max", "50000 100000"),
    ] {
        if fs::read_to_string(budget.join(file))?.trim() != expected {
            return Err(Error::Evidence);
        }
    }
    if super::Events::parse(&fs::read(session.role(Role::Core).join("memory.events"))?)
        .map_err(super::Error::from)?
        .oom_kill
        != 0
    {
        return Err(Error::Evidence);
    }
    Ok(Receipt {
        scope: Scope::Primitive,
        daemon_global_uid: 100_000,
        ancestor_budget_immutable: true,
        job_oom_contained: true,
        core_survived: true,
    })
}
pub fn run(root: &Path, util: &Path) -> Result<Receipt, Error> {
    let util = Package::parse(util)?;
    let (mut session, plan) = Session::admit(root)?;
    let result = session
        .prepare(plan, Budget::Mapped)
        .map_err(Error::from)
        .and_then(|()| observe(&mut session, &util));
    match (result, session.cleanup()) {
        (Ok(receipt), Ok(())) => Ok(receipt),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
        (Err(primary), Err(cleanup)) => Err(Error::Combined {
            primary: Box::new(primary),
            cleanup: Box::new(cleanup),
        }),
    }
}
fn refuse_budget_writes(view: &Path) -> Result<(), Error> {
    for control in ["memory.max", "pids.max", "cpu.max"] {
        let path = view.join(control);
        let before = fs::read(&path)?;
        match Session::write(&path, &before) {
            Ok(()) => return Err(Error::Evidence),
            Err(super::Error::Io(error)) if error.kind() == io::ErrorKind::PermissionDenied => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
enum Undo {
    Mount,
    Supervisor,
    ReturnToRoot,
    Delegation,
    Job,
}
fn namespace_diagnostic(view: &Path) -> Result<(), Error> {
    let mut stderr = io::stderr().lock();
    for name in ["uid_map", "gid_map", "cgroup"] {
        writeln!(
            stderr,
            "mapped self {name}: {}",
            fs::read_to_string(Path::new("/proc/self").join(name))?
        )?;
    }
    for line in fs::read_to_string("/proc/self/status")?.lines() {
        if [
            "Uid:",
            "Gid:",
            "CapEff:",
            "CapPrm:",
            "NoNewPrivs:",
            "NSpid:",
            "Threads:",
        ]
        .iter()
        .any(|prefix| line.starts_with(prefix))
        {
            writeln!(stderr, "mapped self {line}")?;
        }
    }
    for name in ["user", "cgroup", "mnt", "pid"] {
        writeln!(
            stderr,
            "mapped self {name} namespace: {}",
            fs::read_link(Path::new("/proc/self/ns").join(name))?.display()
        )?;
    }
    let metadata = fs::metadata(view)?;
    writeln!(
        stderr,
        "mapped target uid={} mode={:o}",
        metadata.uid(),
        metadata.mode()
    )?;
    Ok(())
}
fn delegate(view: &Path, util: &Package) -> Result<(), Error> {
    if !rustix::process::getuid().is_root()
        || fs::read_to_string("/proc/self/uid_map")?
            .split_whitespace()
            .collect::<Vec<_>>()
            != ["0", "100000", "65536"]
    {
        return Err(Error::Mapping);
    }
    let mut undo = Vec::new();
    let mut child = None;
    let result = (|| {
        if let Err(primary) = util.call(
            Tool::Mount,
            &[
                "-t".as_ref(),
                "cgroup2".as_ref(),
                "none".as_ref(),
                view.as_os_str(),
            ],
        ) {
            if let Err(cleanup) = namespace_diagnostic(view) {
                return Err(Error::Diagnostic {
                    primary: Box::new(primary),
                    diagnostic: Box::new(cleanup),
                });
            }
            return Err(primary);
        }
        undo.push(Undo::Mount);
        if fs::metadata(view.join("memory.max"))?.uid() != 65_534 {
            return Err(Error::BudgetOwner);
        }
        refuse_budget_writes(view)?;
        Session::mkdir(&view.join("supervisor"))?;
        undo.push(Undo::Supervisor);
        undo.push(Undo::ReturnToRoot);
        Session::write(&view.join("supervisor/cgroup.procs"), b"0")?;
        undo.push(Undo::Delegation);
        Session::write(&view.join("cgroup.subtree_control"), b"+cpu +memory +pids")?;
        Session::mkdir(&view.join("job"))?;
        undo.push(Undo::Job);
        for (file, value) in [
            ("memory.max", "33554432"),
            ("pids.max", "8"),
            ("cpu.max", "25000 100000"),
        ] {
            Session::write(&view.join("job").join(file), value.as_bytes())?;
        }
        child = Some(
            Command::new(std::env::current_exe()?)
                .args(["operator-probe", "mapped-job"])
                .arg(view)
                .arg(&util.0)
                .spawn()?,
        );
        let job = child.as_mut().ok_or(Error::Evidence)?;
        if Session::wait(job, Duration::from_secs(30))?.signal() != Some(9)
            || super::Events::parse(&fs::read(view.join("job/memory.events"))?)
                .map_err(super::Error::from)?
                .oom_kill
                == 0
        {
            return Err(Error::Evidence);
        }
        Ok(())
    })();
    let mut issues = Vec::new();
    if let Some(mut child) = child
        && let Err(error) = Session::stop(&mut child)
    {
        issues.push(error.to_string());
    }
    while let Some(action) = undo.pop() {
        let result: Result<(), Error> = match action {
            Undo::Mount => util.call(Tool::Umount, &[view.as_os_str()]),
            Undo::Supervisor => fs::remove_dir(view.join("supervisor")).map_err(Error::from),
            Undo::ReturnToRoot => {
                Session::write(&view.join("cgroup.procs"), b"0").map_err(Error::from)
            }
            Undo::Delegation => {
                Session::write(&view.join("cgroup.subtree_control"), b"-cpu -memory -pids")
                    .map_err(Error::from)
            }
            Undo::Job => fs::remove_dir(view.join("job")).map_err(Error::from),
        };
        if let Err(error) = result {
            issues.push(error.to_string());
        }
    }
    if !issues.is_empty() {
        return Err(Error::Cleanup {
            primary: result.err().map(Box::new),
            issues,
        });
    }
    result?;
    writeln!(
        io::stdout().lock(),
        "MAPPED_DAEMON_CANNOT_RELAX_BUDGET_AND_JOB_OOM_CONTAINED"
    )?;
    Ok(())
}
#[derive(Clone, Copy)]
pub enum Worker {
    Bootstrap,
    Delegate,
    Job,
}
impl Worker {
    pub fn parse(value: &std::ffi::OsStr) -> Result<Self, Error> {
        match value.to_str() {
            Some("mapped-bootstrap") => Ok(Self::Bootstrap),
            Some("mapped-delegate") => Ok(Self::Delegate),
            Some("mapped-job") => Ok(Self::Job),
            _ => Err(Error::Usage),
        }
    }
}
#[cfg(test)]
mod test;
pub fn worker(operation: Worker, args: &[OsString]) -> Result<(), Error> {
    match (operation, args) {
        (Worker::Bootstrap, [budget, view, util]) => {
            if !rustix::process::getuid().is_root()
                || rustix::process::getpid().as_raw_nonzero().get() != 1
                || fs::read_link("/proc/self/ns/mnt")? == fs::read_link("/proc/1/ns/mnt")?
            {
                return Err(Error::Namespace);
            }
            let util = Package::parse(Path::new(util))?;
            util.call(
                Tool::Mount,
                &[
                    "-t".as_ref(),
                    "proc".as_ref(),
                    "proc".as_ref(),
                    "/proc".as_ref(),
                ],
            )?;
            Session::write(&Path::new(budget).join("cgroup.procs"), b"0")?;
            Err(Command::new(util.binary(Tool::Unshare))
                .args([
                    "--user",
                    "--map-users",
                    "0:100000:65536",
                    "--map-groups",
                    "0:100000:65536",
                    "--setuid",
                    "0",
                    "--setgid",
                    "0",
                    "--cgroup",
                    "--mount",
                    "--propagation",
                    "private",
                ])
                .arg(std::env::current_exe()?)
                .args(["operator-probe", "mapped-delegate"])
                .arg(view)
                .arg(&util.0)
                .exec()
                .into())
        }
        (Worker::Delegate, [view, util]) => {
            delegate(Path::new(view), &Package::parse(Path::new(util))?)
        }
        (Worker::Job, [view, util]) => {
            let util = Package::parse(Path::new(util))?;
            Session::write(&Path::new(view).join("job/cgroup.procs"), b"0")?;
            Err(Command::new(util.binary(Tool::Setpriv))
                .args(["--reuid=30001", "--regid=30000", "--clear-groups"])
                .arg(std::env::current_exe()?)
                .args(["operator-probe", "worker", "mapped-memory"])
                .exec()
                .into())
        }
        _ => Err(Error::Usage),
    }
}
