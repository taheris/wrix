use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        process::{CommandExt, ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// usage: wrix-public-run --cli ABSOLUTE-EXECUTABLE PROFILE
    Usage,
    /// public run profile or executable is not a regular file
    File,
    /// public run I/O failed: {0}
    Io(#[from] io::Error),
    /// public run terminal setup failed: {0}
    Terminal(#[from] rustix::io::Errno),
    /// public run child has no observed completion status
    Status,
}
struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        let cleanup = (|| -> io::Result<()> {
            if self.0.try_wait()?.is_none() {
                self.0.kill()?;
                self.0.wait()?;
            }
            Ok(())
        })();
        if let Err(error) = cleanup
            && writeln!(
                io::stderr().lock(),
                "public run child cleanup failed: {error}"
            )
            .is_err()
        {
            std::process::exit(74);
        }
    }
}
fn paths(cli: &Path, profile: &Path) -> Result<(PathBuf, PathBuf), Error> {
    if !cli.is_absolute() {
        return Err(Error::Usage);
    }
    let profile = std::fs::canonicalize(profile)?;
    if !profile.is_file() || !cli.is_file() {
        return Err(Error::File);
    }
    Ok((cli.to_owned(), profile))
}
fn session(cli: &Path, profile: &Path) -> Result<u8, Error> {
    let (cli, profile) = paths(cli, profile)?;
    rustix::process::setsid()?;
    rustix::process::ioctl_tiocsctty(io::stdin())?;
    Err(Command::new(cli)
        .arg("--profile-config")
        .arg(profile)
        .args([
            "run",
            "--nix-store",
            "shared",
            "--no-cache",
            "--",
            "public-second",
        ])
        .exec()
        .into())
}
fn parent(cli: &Path, profile: &Path) -> Result<u8, Error> {
    let (cli, profile) = paths(cli, profile)?;
    let descriptor =
        rustix::pty::openpt(rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY)?;
    rustix::io::fcntl_setfd(&descriptor, rustix::io::FdFlags::CLOEXEC)?;
    rustix::pty::grantpt(&descriptor)?;
    rustix::pty::unlockpt(&descriptor)?;
    let name = rustix::pty::ptsname(&descriptor, Vec::new())?;
    let slave = OpenOptions::new()
        .read(true)
        .write(true)
        .open(Path::new(std::ffi::OsStr::from_bytes(name.as_bytes())))?;
    let mut master = File::from(descriptor);
    let mut child = Owned(
        Command::new(std::env::current_exe()?)
            .arg("--session")
            .arg(cli)
            .arg(profile)
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave))
            .spawn()?,
    );
    let copied = (|| -> io::Result<()> {
        let mut output = io::stdout().lock();
        let mut bytes = vec![0; 65_536].into_boxed_slice();
        loop {
            match master.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => output.write_all(&bytes[..count])?,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error)
                    if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) =>
                {
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        output.flush()
    })();
    drop(master);
    copied?;
    let status = child.0.wait()?;
    let code = status
        .code()
        .or_else(|| status.signal().map(|signal| -signal))
        .ok_or(Error::Status)?;
    u8::try_from(code.rem_euclid(256)).map_err(|_| Error::Status)
}
pub fn run(args: &[OsString]) -> Result<u8, Error> {
    let [operation, cli, profile] = args else {
        return Err(Error::Usage);
    };
    if operation == "--session" {
        session(Path::new(cli), Path::new(profile))
    } else if operation == "--cli" {
        parent(Path::new(cli), Path::new(profile))
    } else {
        Err(Error::Usage)
    }
}
