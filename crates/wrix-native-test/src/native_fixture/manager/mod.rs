use super::{command, process, root};
use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// private fixture manager filesystem failed: {0}
    Io(#[from] io::Error),
    /// private fixture manager ownership failed: {0}
    Root(#[from] root::Error),
    /// private fixture manager receipt failed: {0}
    Json(#[from] serde_json::Error),
    /// private fixture manager observation failed: {0}
    Process(#[from] process::Error),
    /// private fixture manager command failed: {0}
    Command(#[from] command::Error),
    /// {0}
    Policy(&'static str),
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    version: u8,
    pid: u32,
    cgroup: PathBuf,
    device: u64,
    inode: u64,
    executable: PathBuf,
    start_ticks: String,
}

pub struct Manager {
    proof: Proof,
    birth: u64,
}
impl Manager {
    pub fn read(subject: &root::Root) -> Result<Option<Self>, Error> {
        let path = subject.child("private-user-manager/ownership.json")?;
        let file = match root::open_private(&path, 65536) {
            Ok(file) => file,
            Err(root::Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let proof: Proof = serde_json::from_reader(file)?;
        let name = subject
            .path()
            .file_name()
            .ok_or(Error::Policy("fixture root has no name"))?
            .as_encoded_bytes();
        let expected = [b"wx-doaia-public-".as_slice(), name, b".scope"].concat();
        if proof.version != 1
            || !proof.cgroup.starts_with("/sys/fs/cgroup")
            || proof.cgroup.components().any(|part| {
                !matches!(
                    part,
                    std::path::Component::RootDir | std::path::Component::Normal(_)
                )
            })
            || proof
                .cgroup
                .file_name()
                .map(std::ffi::OsStr::as_encoded_bytes)
                != Some(expected.as_slice())
        {
            return Err(Error::Policy("invalid owned private manager cgroup"));
        }
        match fs::metadata(&proof.cgroup) {
            Ok(metadata) => {
                if fs::canonicalize(&proof.cgroup)? != proof.cgroup
                    || metadata.uid() != rustix::process::getuid().as_raw()
                    || (metadata.dev(), metadata.ino()) != (proof.device, proof.inode)
                {
                    return Err(Error::Policy("private manager cgroup identity changed"));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let birth = proof
            .start_ticks
            .parse()
            .map_err(|_| Error::Policy("invalid private manager birth identity"))?;
        Ok(Some(Self { proof, birth }))
    }
    pub fn alive(&self) -> Result<bool, Error> {
        let proc = PathBuf::from(format!("/proc/{}", self.proof.pid));
        let observation = (|| {
            let stat = process::Record::parse(&fs::read(proc.join("stat"))?)?;
            if stat.start_ticks != self.birth {
                return Err(Error::Policy("private manager birth identity changed"));
            }
            if stat.state == b'Z' {
                return Ok(false);
            }
            if fs::canonicalize(proc.join("exe"))? != self.proof.executable {
                return Err(Error::Policy("private manager process identity changed"));
            }
            let membership = fs::read_to_string(proc.join("cgroup"))?;
            let relative = membership
                .trim()
                .strip_prefix("0::/")
                .ok_or(Error::Policy("invalid private manager cgroup membership"))?;
            let actual = Path::new("/sys/fs/cgroup").join(relative);
            if actual
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
                || !actual.starts_with(&self.proof.cgroup)
            {
                return Err(Error::Policy("private manager escaped its owned cgroup"));
            }
            Ok(true)
        })();
        match observation {
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            result => result,
        }
    }
    pub fn stop_controller(
        &self,
        subject: &root::Root,
        environment: &command::Environment,
        tools: Option<&Path>,
    ) -> Result<(), Error> {
        if !self.alive()? {
            return Ok(());
        }
        let tools = tools
            .filter(|path| path.is_absolute())
            .ok_or(Error::Policy(
                "private manager teardown requires fixed package tools",
            ))?;
        let selected = command::checked(
            environment.command(tools.join("systemctl")).args([
                "--user",
                "show",
                "wrix-host-controller.service",
                "--property=FragmentPath",
                "--value",
            ]),
            Duration::from_secs(45),
        )?;
        let selected =
            String::from_utf8(selected).map_err(|_| Error::Policy("invalid controller path"))?;
        if selected.trim().is_empty() {
            return Ok(());
        }
        let selected = Path::new(selected.trim());
        let expected =
            subject.child("home/.local/state/wrix/control/wrix-host-controller.service")?;
        let linked = subject.child("home/.config/systemd/user/wrix-host-controller.service")?;
        if selected != expected && selected != linked || fs::canonicalize(selected)? != expected {
            return Err(Error::Policy("foreign controller in private test manager"));
        }
        let actual = fs::metadata(selected)?;
        let original = fs::metadata(&expected)?;
        if (actual.dev(), actual.ino()) != (original.dev(), original.ino()) {
            return Err(Error::Policy("foreign controller in private test manager"));
        }
        command::checked(
            environment.command(tools.join("systemctl")).args([
                "--user",
                "stop",
                "wrix-host-controller.service",
            ]),
            Duration::from_secs(45),
        )?;
        Ok(())
    }
    pub fn stop(
        &self,
        subject: &root::Root,
        environment: &command::Environment,
        tools: Option<&Path>,
    ) -> Result<(), Error> {
        if !self.proof.cgroup.try_exists()? {
            if self.alive()? {
                return Err(Error::Policy("private manager lost its owned cgroup"));
            }
            return Ok(());
        }
        let fresh =
            Self::read(subject)?.ok_or(Error::Policy("private manager proof disappeared"))?;
        if (fresh.proof.device, fresh.proof.inode) != (self.proof.device, self.proof.inode) {
            return Err(Error::Policy("private manager cgroup identity changed"));
        }
        if self.alive()? {
            let tools = tools
                .filter(|path| path.is_absolute())
                .ok_or(Error::Policy(
                    "private manager teardown requires fixed package tools",
                ))?;
            for arguments in [
                vec!["--user", "stop", "dbus.socket", "dbus.service"],
                vec!["--user", "--force", "exit"],
            ] {
                command::checked(
                    environment.command(tools.join("systemctl")).args(arguments),
                    Duration::from_secs(45),
                )?;
            }
        }
        fs::write(self.proof.cgroup.join("cgroup.kill"), "1\n")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while fs::read_to_string(self.proof.cgroup.join("cgroup.events"))?.contains("populated 1") {
            if Instant::now() >= deadline {
                return Err(Error::Policy("private manager subtree remains populated"));
            }
            thread::sleep(Duration::from_millis(20));
        }
        remove_directories(&self.proof.cgroup)?;
        Ok(())
    }
}
#[cfg(test)]
mod test;

fn remove_directories(path: &Path) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            remove_directories(&entry.path())?;
        }
    }
    fs::remove_dir(path)
}
