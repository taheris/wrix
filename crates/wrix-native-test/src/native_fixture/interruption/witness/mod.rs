use super::{Error, Progress, write_json};
use crate::native_fixture::{cleanup, mount, root};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{self, BufRead, Read, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    version: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mountinfo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    running: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    containers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}
impl Snapshot {
    const fn empty() -> Self {
        Self {
            version: 1,
            namespace: None,
            mountinfo: None,
            running: None,
            containers: None,
            images: None,
            error: None,
        }
    }
    const fn valid(&self) -> Result<(), Error> {
        if self.version != 1 || self.error.is_some() {
            Err(Error::Policy("namespace inspection failed"))
        } else {
            Ok(())
        }
    }
    fn table(&self) -> Result<Vec<mount::Entry>, Error> {
        Ok(mount::parse(
            self.mountinfo
                .as_ref()
                .ok_or(Error::Policy("missing namespace mount table"))?
                .as_bytes(),
        )?)
    }
}

pub struct Witness {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    buffer: Vec<u8>,
    closed: bool,
}
impl Witness {
    pub fn spawn(subject: &cleanup::Subject, log: File) -> Result<Self, Error> {
        let mut args = subject.podman_args([
            std::ffi::OsString::from("unshare"),
            std::env::current_exe()?.into_os_string(),
            "native-interruption".into(),
            "--witness".into(),
            "--root".into(),
            subject.root.path().as_os_str().to_owned(),
        ]);
        if let Some(tools) = &subject.tools {
            args.extend(["--tools".into(), tools.as_os_str().to_owned()]);
        }
        let (program, args) = args
            .split_first()
            .ok_or(Error::Policy("missing namespace witness command"))?;
        let mut child = subject
            .environment
            .command(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(log)
            .process_group(0)
            .spawn()?;
        let input = child
            .stdin
            .take()
            .ok_or(Error::Policy("missing namespace witness stdin"))?;
        let output = child
            .stdout
            .take()
            .ok_or(Error::Policy("missing namespace witness stdout"))?;
        let witness = Self {
            child,
            input,
            output,
            buffer: Vec::new(),
            closed: false,
        };
        rustix::fs::fcntl_setfl(&witness.output, rustix::fs::OFlags::NONBLOCK)?;
        Ok(witness)
    }
    pub const fn closed(&self) -> bool {
        self.closed
    }
    pub fn snapshot(&mut self, path: &Path, progress: &Progress) -> Result<Snapshot, Error> {
        self.input.write_all(b"snapshot\n")?;
        self.input.flush()?;
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut next = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let line = self.buffer.drain(..=end).collect::<Vec<_>>();
                let snapshot: Snapshot = serde_json::from_slice(&line)?;
                write_json(path, &snapshot)?;
                snapshot.valid()?;
                return Ok(snapshot);
            }
            let mut chunk = [0; 8192];
            match self.output.read(&mut chunk) {
                Ok(0) => return Err(Error::Policy("original Podman namespace witness exited")),
                Ok(count) => {
                    self.buffer.extend_from_slice(&chunk[..count]);
                    if self.buffer.len() > 8 * 1024 * 1024 {
                        return Err(Error::Policy("oversized namespace snapshot"));
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error.into()),
            }
            if Instant::now() >= deadline {
                return Err(Error::Deadline("original Podman namespace inspection"));
            }
            if Instant::now() >= next {
                progress.say("Inspecting retained original Podman namespace (bounded)")?;
                next = Instant::now() + Duration::from_secs(15);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn wait(&mut self, duration: Duration) -> Result<Option<std::process::ExitStatus>, Error> {
        let deadline = Instant::now() + duration;
        loop {
            if let Some(status) = self.child.try_wait()? {
                self.closed = true;
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn terminate(&self, kind: rustix::process::Signal) -> Result<(), Error> {
        let pid = rustix::process::Pid::from_raw(
            i32::try_from(self.child.id())
                .map_err(|_| Error::Policy("invalid namespace observer identity"))?,
        )
        .ok_or(Error::Policy("invalid namespace observer identity"))?;
        match rustix::process::kill_process_group(pid, kind) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
    pub fn close(&mut self, progress: &Progress) -> Result<(), Error> {
        if self.closed {
            return Ok(());
        }
        progress.say("Releasing the original namespace observer")?;
        match self
            .input
            .write_all(b"exit\n")
            .and_then(|()| self.input.flush())
        {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}
            Err(error) => return Err(error.into()),
        }
        if let Some(status) = self.wait(Duration::from_secs(10))? {
            return if status.success() {
                Ok(())
            } else {
                Err(Error::Policy("namespace witness exited unsuccessfully"))
            };
        }
        progress.say("Namespace observer timed out; terminating only its owned process group")?;
        self.terminate(rustix::process::Signal::TERM)?;
        if self.wait(Duration::from_secs(5))?.is_none() {
            progress
                .say("Namespace observer ignored TERM; killing only its owned process group")?;
            self.terminate(rustix::process::Signal::KILL)?;
            if self.wait(Duration::from_secs(5))?.is_none() {
                return Err(Error::Policy(
                    "namespace witness remained alive after bounded KILL",
                ));
            }
        }
        Err(Error::Policy("namespace witness required termination"))
    }
}
impl Drop for Witness {
    fn drop(&mut self) {
        if !self.closed {
            let cleanup = (|| {
                self.terminate(rustix::process::Signal::KILL)?;
                self.child.wait()?;
                self.closed = true;
                Ok::<_, Error>(())
            })();
            if let Err(error) = cleanup {
                tracing::error!(%error,"owned namespace witness cleanup failed");
            }
        }
    }
}

fn text(bytes: Vec<u8>) -> Result<String, Error> {
    String::from_utf8(bytes).map_err(|_| Error::Policy("invalid UTF-8 namespace observation"))
}
pub fn serve(root: &Path, tools: Option<std::path::PathBuf>) -> Result<(), Error> {
    let subject = cleanup::Subject::new(root::Root::parse(root, root::Origin::Registered)?, tools)?;
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        let mut bytes = Vec::new();
        let count = Read::by_ref(&mut input)
            .take(129)
            .read_until(b'\n', &mut bytes)?;
        if count == 0 {
            return Ok(());
        }
        if bytes == b"exit\n" {
            return Ok(());
        }
        if bytes != b"snapshot\n" {
            return Err(Error::Policy("unsupported namespace observation request"));
        }
        let mut snapshot = Snapshot::empty();
        let outcome = (|| {
            let timeout = Duration::from_secs(10);
            let mut runtime = cleanup::Native;
            snapshot.namespace = Some(
                text(subject.invoke(
                    &mut runtime,
                    &[subject.program("readlink"), "/proc/self/ns/mnt".into()],
                    timeout,
                )?)?
                .trim()
                .to_owned(),
            );
            snapshot.mountinfo = Some(text(subject.invoke(
                &mut runtime,
                &[subject.program("cat"), "/proc/self/mountinfo".into()],
                timeout,
            )?)?);
            snapshot.running = Some(
                text(subject.invoke(
                    &mut runtime,
                    &subject.podman_args(["ps", "--no-trunc", "--format", "{{.ID}}"]),
                    timeout,
                )?)?
                .lines()
                .map(str::to_owned)
                .collect(),
            );
            snapshot.containers = Some(
                text(subject.invoke(
                    &mut runtime,
                    &subject.podman_args(["ps", "--all", "--no-trunc", "--format", "{{.ID}}"]),
                    timeout,
                )?)?
                .lines()
                .map(str::to_owned)
                .collect(),
            );
            snapshot.images = Some(
                text(subject.invoke(
                    &mut runtime,
                    &subject.podman_args(["images", "--no-trunc", "--format", "{{.ID}}"]),
                    timeout,
                )?)?
                .lines()
                .map(str::to_owned)
                .collect(),
            );
            Ok::<_, Error>(())
        })();
        if let Err(error) = outcome {
            snapshot.error = Some(error.to_string());
        }
        serde_json::to_writer(&mut output, &snapshot)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
}

pub fn verify_before(subject: &cleanup::Subject, snapshot: &Snapshot) -> Result<(), Error> {
    snapshot.valid()?;
    let namespace = snapshot
        .namespace
        .as_deref()
        .ok_or(Error::Policy("missing native namespace identity"))?;
    let identifier = namespace
        .strip_prefix("mnt:[")
        .and_then(|value| value.strip_suffix(']'))
        .ok_or(Error::Policy(
            "Podman witness is not in its own mount namespace",
        ))?;
    if identifier.is_empty()
        || !identifier.bytes().all(|byte| byte.is_ascii_digit())
        || fs::read_link("/proc/self/ns/mnt")?.as_os_str() == namespace
    {
        return Err(Error::Policy(
            "Podman witness is not in its own mount namespace",
        ));
    }
    let running = snapshot
        .running
        .as_ref()
        .ok_or(Error::Policy("missing running container inventory"))?;
    if running.iter().collect::<BTreeSet<_>>().len() < 3
        || running.iter().any(|value| {
            value.len() != 64
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return Err(Error::Policy(
            "native pool and two workers are not all running",
        ));
    }
    if mount::views(subject.root.path(), &snapshot.table()?)?
        .into_iter()
        .collect::<BTreeSet<_>>()
        .len()
        < 2
    {
        return Err(Error::Policy(
            "two native worker exports are not mounted in Podman's namespace",
        ));
    }
    subject.check_host_mounts(&mut cleanup::Native)?;
    Ok(())
}
pub fn verify_after(
    subject: &cleanup::Subject,
    before: &Snapshot,
    after: &Snapshot,
) -> Result<(), Error> {
    after.valid()?;
    if after.namespace.is_none() || after.namespace != before.namespace {
        return Err(Error::Policy(
            "namespace changed: refusing to inspect a replacement empty namespace",
        ));
    }
    for value in [&after.running, &after.containers, &after.images] {
        if !value
            .as_ref()
            .ok_or(Error::Policy("missing final resource inventory"))?
            .is_empty()
        {
            return Err(Error::Policy(
                "fixture still owns containers or images after teardown",
            ));
        }
    }
    mount::namespace_clear(subject.root.path(), &after.table()?)?;
    subject.check_host_mounts(&mut cleanup::Native)?;
    if !subject.root.stopped()? {
        return Err(Error::Policy(
            "native supervisor did not record successful teardown",
        ));
    }
    Ok(())
}
