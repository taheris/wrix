pub mod entry;
#[cfg(test)]
mod test;

use super::{command, manager, mount, root};
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs::{self, DirBuilder},
    io::{self, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt},
    },
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// native fixture cleanup filesystem failed: {0}
    Io(#[from] io::Error),
    /// native fixture filesystem operation failed: {0}
    Filesystem(#[from] rustix::io::Errno),
    /// {0}
    Root(#[from] root::Error),
    /// {0}
    Mount(#[from] mount::Error),
    /// {0}
    Manager(#[from] manager::Error),
    /// {0}
    Command(#[from] command::Error),
    /// {0}
    Observation(#[from] super::process::Error),
    /// {0}
    Policy(&'static str),
    /// native child signalling requires Linux (child={pid}, parent={parent})
    SignalPlatform { pid: u32, parent: u32 },
    /// fixture still has host mounts: {0}
    Mounted(PathBuf),
}

pub trait Runtime {
    fn host_mounts(&mut self) -> Result<Vec<mount::Entry>, Error>;
    fn execute(
        &mut self,
        environment: &command::Environment,
        args: &[OsString],
        timeout: Duration,
    ) -> Result<Vec<u8>, Error>;
    fn remove_tree(&mut self, path: &Path) -> Result<(), Error> {
        fs::remove_dir_all(path)?;
        Ok(())
    }
}

pub struct Native;
impl Runtime for Native {
    fn host_mounts(&mut self) -> Result<Vec<mount::Entry>, Error> {
        Ok(mount::parse(&fs::read("/proc/self/mountinfo")?)?)
    }
    fn execute(
        &mut self,
        environment: &command::Environment,
        args: &[OsString],
        timeout: Duration,
    ) -> Result<Vec<u8>, Error> {
        let (program, args) = args
            .split_first()
            .ok_or(Error::Policy("missing fixture cleanup command"))?;
        Ok(command::checked(
            environment.command(program).args(args),
            timeout,
        )?)
    }
}

pub struct Subject {
    pub root: root::Root,
    pub tools: Option<PathBuf>,
    pub environment: command::Environment,
    pub storage: PathBuf,
    pub podman: Vec<OsString>,
    manager: Option<manager::Manager>,
}

impl Subject {
    pub fn new(root: root::Root, tools: Option<PathBuf>) -> Result<Self, Error> {
        let mut environment = command::Environment::private(root.path());
        let manager = manager::Manager::read(&root)?;
        if manager.is_some() {
            environment.set(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", root.child("runtime/bus")?.display()),
            );
        }
        let storage = root.child("home/.local/share/containers/storage")?;
        let program = tools.as_ref().map_or_else(
            || OsString::from("podman"),
            |tools| tools.join("podman").into_os_string(),
        );
        let podman = vec![
            program,
            "--remote=false".into(),
            "--root".into(),
            storage.clone().into_os_string(),
            "--runroot".into(),
            root.child("runtime/containers")?.into_os_string(),
            "--tmpdir".into(),
            root.child("runtime/libpod/tmp")?.into_os_string(),
        ];
        Ok(Self {
            root,
            tools,
            environment,
            storage,
            podman,
            manager,
        })
    }
    pub fn program(&self, name: &str) -> OsString {
        self.tools
            .as_ref()
            .map_or_else(|| name.into(), |tools| tools.join(name).into_os_string())
    }
    pub fn invoke(
        &self,
        runtime: &mut impl Runtime,
        args: &[OsString],
        timeout: Duration,
    ) -> Result<Vec<u8>, Error> {
        self.root.recheck()?;
        runtime.execute(&self.environment, args, timeout)
    }
    pub fn podman_args(
        &self,
        tail: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Vec<OsString> {
        let mut args = self.podman.clone();
        args.extend(tail.into_iter().map(Into::into));
        args
    }
    pub fn check_host_mounts(&self, runtime: &mut impl Runtime) -> Result<(), Error> {
        if runtime
            .host_mounts()?
            .iter()
            .any(|entry| entry.target.starts_with(self.root.path()))
        {
            return Err(Error::Mounted(self.root.path().to_owned()));
        }
        Ok(())
    }
    fn diagnostics(&self, runtime: &mut impl Runtime) -> Result<(), Error> {
        let directory = self.root.child("native-cleanup-diagnostics")?;
        match DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        root::private_directory(&directory)?;
        let identifiers = self.invoke(
            runtime,
            &self.podman_args(["ps", "--all", "--no-trunc", "--format", "{{.ID}}"]),
            Duration::from_secs(3),
        )?;
        for identifier in identifiers
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            if identifier.len() != 64
                || !identifier
                    .iter()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
            {
                return Err(Error::Policy(
                    "invalid fixture container identity for diagnostics",
                ));
            }
            let identifier = std::str::from_utf8(identifier)
                .map_err(|_| Error::Policy("invalid fixture container identity"))?;
            for (label, arguments) in [
                (
                    "state",
                    vec!["inspect", "--format", "{{json .State}}", identifier],
                ),
                ("log", vec!["logs", "--tail=200", identifier]),
            ] {
                let output = self.invoke(
                    runtime,
                    &self.podman_args(arguments),
                    Duration::from_secs(3),
                )?;
                let mut file = tempfile::Builder::new()
                    .prefix(&format!("{identifier}-{label}-"))
                    .tempfile_in(&directory)?;
                file.write_all(&output)?;
                file.keep().map_err(|error| error.error)?;
            }
        }
        Ok(())
    }
    pub fn finish(&self, runtime: &mut impl Runtime, diagnostics: bool) -> Result<(), Error> {
        if self.root.stopped()? {
            return self.check_host_mounts(runtime);
        }
        if let Some(manager) = &self.manager {
            manager.stop_controller(&self.root, &self.environment, self.tools.as_deref())?;
        }
        let storage = self.root.child("home/.local/share/containers/storage")?;
        let has_storage = storage.try_exists()?;
        if has_storage {
            let directory = self.root.child("runtime")?;
            match DirBuilder::new().mode(0o700).create(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            root::private_directory(&directory)?;
            if diagnostics && let Err(error) = self.diagnostics(runtime) {
                writeln!(
                    io::stderr().lock(),
                    "could not capture fixture diagnostics; continuing resource teardown: {error}"
                )?;
            }
            self.invoke(
                runtime,
                &self.podman_args(["stop", "--all", "--time=10"]),
                Duration::from_secs(45),
            )?;
        }
        for view in mount::views(self.root.path(), &runtime.host_mounts()?)? {
            let helper = [
                "/run/wrappers/bin/fusermount3",
                "/usr/bin/fusermount3",
                "/bin/fusermount3",
            ]
            .into_iter()
            .find(|path| rustix::fs::access(*path, rustix::fs::Access::EXEC_OK).is_ok())
            .unwrap_or("fusermount3");
            self.invoke(
                runtime,
                &[
                    helper.into(),
                    "-u".into(),
                    "--".into(),
                    view.into_os_string(),
                ],
                Duration::from_secs(45),
            )?;
        }
        if has_storage {
            let mounts_args = self.podman_args([
                OsString::from("unshare"),
                self.program("cat"),
                "/proc/self/mountinfo".into(),
            ]);
            let table =
                mount::parse(&self.invoke(runtime, &mounts_args, Duration::from_secs(45))?)?;
            for view in mount::views(self.root.path(), &table)? {
                self.invoke(
                    runtime,
                    &self.podman_args([
                        OsString::from("unshare"),
                        self.program("fusermount3"),
                        "-u".into(),
                        "--".into(),
                        view.into_os_string(),
                    ]),
                    Duration::from_secs(45),
                )?;
            }
            for operation in ["rm", "rmi"] {
                self.invoke(
                    runtime,
                    &self.podman_args([operation, "--all"]),
                    Duration::from_secs(45),
                )?;
            }
            let table =
                mount::parse(&self.invoke(runtime, &mounts_args, Duration::from_secs(45))?)?;
            mount::namespace_clear(self.root.path(), &table)?;
            self.root.child("home/.local/share/containers/storage")?;
            self.invoke(
                runtime,
                &self.podman_args([
                    OsString::from("unshare"),
                    self.program("chown"),
                    "--recursive".into(),
                    "--no-dereference".into(),
                    "0:0".into(),
                    storage.clone().into_os_string(),
                ]),
                Duration::from_secs(45),
            )?;
            self.invoke(
                runtime,
                &self.podman_args([
                    OsString::from("unshare"),
                    self.program("chmod"),
                    "--recursive".into(),
                    "u+rwX".into(),
                    storage.into_os_string(),
                ]),
                Duration::from_secs(45),
            )?;
            self.invoke(
                runtime,
                &self.podman_args(["system", "migrate"]),
                Duration::from_secs(45),
            )?;
        }
        self.check_host_mounts(runtime)?;
        if let Some(manager) = &self.manager {
            manager.stop(&self.root, &self.environment, self.tools.as_deref())?;
        }
        self.root.record_stopped()?;
        Ok(())
    }
    pub fn remove_files(&self, runtime: &mut impl Runtime) -> Result<(), Error> {
        if !self.root.stopped()? {
            return Err(Error::Policy(
                "fixture teardown must complete before deleting files",
            ));
        }
        self.check_host_mounts(runtime)?;
        let storage = self.root.child("home/.local/share/containers/storage")?;
        if storage.try_exists()? {
            runtime.remove_tree(&storage)?;
        }
        for entry in fs::read_dir(self.root.path())? {
            let entry = entry?;
            if [
                OsStr::new(root::MARKER),
                OsStr::new(root::STOPPED),
                OsStr::new("runtime"),
            ]
            .contains(&entry.file_name().as_os_str())
            {
                continue;
            }
            if entry.file_type()?.is_dir() {
                runtime.remove_tree(&entry.path())?;
            } else {
                fs::remove_file(entry.path())?;
            }
        }
        let directory = self.root.child("runtime")?;
        if directory.try_exists()? {
            writable_directories(&directory)?;
            runtime.remove_tree(&directory)?;
        }
        match fs::remove_file(self.root.path().join(root::MARKER)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        fs::remove_file(self.root.path().join(root::STOPPED))?;
        fs::remove_dir(self.root.path())?;
        Ok(())
    }
}

fn writable_directories(path: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Ok(());
    }
    if metadata.uid() != rustix::process::getuid().as_raw() {
        return Err(Error::Policy(
            "foreign directory in stopped fixture runtime",
        ));
    }
    #[cfg(target_os = "linux")]
    let directory = std::fs::File::from(rustix::fs::open(
        path,
        rustix::fs::OFlags::PATH
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?);
    #[cfg(target_os = "linux")]
    let pinned = {
        use std::os::fd::AsRawFd;
        let actual = directory.metadata()?;
        if (actual.dev(), actual.ino(), actual.uid())
            != (metadata.dev(), metadata.ino(), metadata.uid())
        {
            return Err(Error::Policy("stopped fixture directory identity changed"));
        }
        let pinned = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
        rustix::fs::chmodat(
            rustix::fs::CWD,
            &pinned,
            rustix::fs::Mode::from_raw_mode(metadata.mode()) | rustix::fs::Mode::RWXU,
            rustix::fs::AtFlags::empty(),
        )?;
        pinned
    };
    #[cfg(not(target_os = "linux"))]
    let pinned = {
        rustix::fs::chmodat(
            rustix::fs::CWD,
            path,
            rustix::fs::Mode::from_raw_mode(
                (metadata.mode() & 0o7777)
                    .try_into()
                    .map_err(|_| Error::Policy("invalid fixture directory mode"))?,
            ) | rustix::fs::Mode::RWXU,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )?;
        path.to_owned()
    };
    for entry in fs::read_dir(&pinned)? {
        writable_directories(&entry?.path())?;
    }
    Ok(())
}

fn legacy_prefix(path: &Path) -> Option<PathBuf> {
    let mut components = path.components();
    if components.next() != Some(std::path::Component::RootDir)
        || components.next()?.as_os_str() != "tmp"
    {
        return None;
    }
    let root = Path::new("/tmp").join(components.next()?.as_os_str());
    root::legacy(&root).then_some(root)
}

pub fn discover(
    runtime: &mut impl Runtime,
    proc: &Path,
    temporary: &Path,
) -> Result<BTreeSet<PathBuf>, Error> {
    let mut selected = BTreeSet::new();
    for entry in runtime.host_mounts()? {
        if entry.kind == b"fuse.sshfs"
            && mount::source(&entry.source)
            && let Some(root) = legacy_prefix(&entry.target)
        {
            selected.insert(root);
        }
    }
    for process in fs::read_dir(proc)? {
        let process = process?;
        if !process
            .file_name()
            .as_encoded_bytes()
            .iter()
            .all(u8::is_ascii_digit)
        {
            continue;
        }
        let args = (|| -> io::Result<Option<Vec<u8>>> {
            if process.metadata()?.uid() != rustix::process::getuid().as_raw() {
                return Ok(None);
            }
            Ok(Some(fs::read(process.path().join("cmdline"))?))
        })();
        let args = match args {
            Ok(Some(args)) => args,
            Ok(None) => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let args: Vec<_> = args.split(|byte| *byte == 0).collect();
        if args
            .first()
            .map(|arg| Path::new(OsStr::from_bytes(arg)).file_name())
            != Some(Some(OsStr::new("conmon")))
        {
            continue;
        }
        let Some(index) = args.iter().position(|arg| *arg == b"-n") else {
            continue;
        };
        if !args.get(index + 1).is_some_and(|arg| mount::source(arg)) {
            continue;
        }
        for arg in args {
            let path = Path::new(OsStr::from_bytes(arg));
            if arg
                .windows(b"/home/.local/share/containers/storage/overlay-containers/".len())
                .any(|piece| piece == b"/home/.local/share/containers/storage/overlay-containers/")
                && let Some(root) = legacy_prefix(path)
            {
                selected.insert(root);
            }
        }
    }
    for entry in fs::read_dir(temporary)? {
        let entry = entry?;
        let path = entry.path();
        if !root::legacy(&path) {
            continue;
        }
        match fs::symlink_metadata(&path) {
            Ok(metadata)
                if metadata.uid() == rustix::process::getuid().as_raw()
                    && (path.join(root::MARKER).is_file()
                        || path.join("home/.local/state/wrix/control/nix").is_dir()) =>
            {
                selected.insert(path);
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(selected)
}
