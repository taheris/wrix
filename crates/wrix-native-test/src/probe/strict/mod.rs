use crate::native_fixture::command;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// strict probe requires immutable tools and absolute paths
    Arguments,
    /// strict probe I/O failed: {0}
    Io(#[from] io::Error),
    /// strict probe expression encoding failed: {0}
    Json(#[from] serde_json::Error),
    /// strict probe process failed: {0}
    Command(#[from] command::Error),
    /// strict probe marker identity changed; foreign data was preserved
    Replaced,
    /// strict probe command did not complete successfully
    Refused,
    /// strict probe cleanup failed ({primary:?}): {cleanup}
    Cleanup {
        primary: Option<Box<Self>>,
        cleanup: Box<Self>,
    },
}
#[derive(Clone, Copy)]
pub enum System {
    Arm,
    Intel,
}
impl System {
    pub(super) const fn wire(self) -> &'static str {
        match self {
            Self::Arm => "aarch64-linux",
            Self::Intel => "x86_64-linux",
        }
    }
    pub fn parse(value: &std::ffi::OsStr) -> Result<Self, Error> {
        match value.to_str() {
            Some("aarch64-linux") => Ok(Self::Arm),
            Some("x86_64-linux") => Ok(Self::Intel),
            _ => Err(Error::Arguments),
        }
    }
}
pub struct Program(PathBuf);
impl Program {
    pub(super) fn root(&self) -> Result<&Path, Error> {
        self.0
            .parent()
            .and_then(Path::parent)
            .ok_or(Error::Arguments)
    }
    pub fn parse(path: &Path) -> Result<Self, Error> {
        let Ok(relative) = path.strip_prefix("/nix/store") else {
            return Err(Error::Arguments);
        };
        let parts: Vec<_> = relative.components().collect();
        let [
            Component::Normal(package),
            Component::Normal(bin),
            Component::Normal(program),
        ] = parts.as_slice()
        else {
            return Err(Error::Arguments);
        };
        let package = package.as_encoded_bytes();
        if package.len() <= 33
            || package[32] != b'-'
            || !package[..32]
                .iter()
                .all(|byte| b"0123456789abcdfghijklmnpqrsvwxyz".contains(byte))
            || !package[33..]
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || b"+-._?=".contains(byte))
            || *bin != "bin"
            || *program != "wrix-native-test"
        {
            return Err(Error::Arguments);
        }
        Ok(Self(path.to_owned()))
    }
}
pub(super) fn quote(text: &str) -> Result<String, Error> {
    if text.chars().any(char::is_control) {
        return Err(Error::Arguments);
    }
    Ok(serde_json::to_string(text)?.replace("${", "\\${"))
}
fn literal(path: &Path) -> Result<String, Error> {
    if !path.is_absolute() {
        return Err(Error::Arguments);
    }
    quote(path.to_str().ok_or(Error::Arguments)?)
}
pub fn expression(program: &Program, marker: &Path, system: System) -> Result<String, Error> {
    let root = program.root()?;
    let root = literal(root)?;
    let marker = literal(marker)?;
    Ok(format!(
        "let probe = builtins.storePath {root}; in derivation {{ name = \"wrix-service-builder-strict-probe\"; system = {}; builder = probe + \"/bin/wrix-native-test\"; args = [\"operator-probe\" \"worker\" \"strict-nix\" {marker}]; }}",
        serde_json::to_string(system.wire())?
    ))
}
#[derive(serde::Serialize)]
pub struct Plan {
    expression: String,
    sandbox: bool,
    sandbox_fallback: bool,
    native_execution: bool,
}
pub fn plan(program: &Program, marker: &Path, system: System) -> Result<Plan, Error> {
    Ok(Plan {
        expression: expression(program, marker, system)?,
        sandbox: true,
        sandbox_fallback: false,
        native_execution: false,
    })
}
struct Marker {
    path: PathBuf,
    device: u64,
    inode: u64,
}
impl Marker {
    fn create(path: &Path) -> Result<Self, Error> {
        let parent = path.parent().ok_or(Error::Arguments)?;
        let metadata = fs::symlink_metadata(parent)?;
        if fs::canonicalize(parent)? != parent
            || !metadata.is_dir()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o022 != 0
        {
            return Err(Error::Arguments);
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        let metadata = file.metadata()?;
        let marker = Self {
            path: path.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        if let Err(primary) = file.write_all(b"probe").and_then(|()| file.sync_all()) {
            return match marker.remove() {
                Ok(()) => Err(primary.into()),
                Err(cleanup) => Err(Error::Cleanup {
                    primary: Some(Box::new(primary.into())),
                    cleanup: Box::new(cleanup),
                }),
            };
        }
        Ok(marker)
    }
    fn remove(self) -> Result<(), Error> {
        let metadata = fs::symlink_metadata(&self.path)?;
        if (metadata.dev(), metadata.ino()) != (self.device, self.inode) {
            return Err(Error::Replaced);
        }
        fs::remove_file(self.path)?;
        Ok(())
    }
}
#[derive(serde::Serialize)]
pub struct Observation {
    pub exit: Option<i32>,
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
pub fn run(
    nix: &Path,
    program: &Program,
    marker: &Path,
    system: System,
) -> Result<Observation, Error> {
    if !nix.is_absolute() {
        return Err(Error::Arguments);
    }
    let expression = expression(program, marker, system)?;
    let marker = Marker::create(marker)?;
    let result = command::capture(
        Command::new(nix).args([
            "build",
            "--impure",
            "--no-link",
            "--print-out-paths",
            "--option",
            "sandbox",
            "true",
            "--option",
            "sandbox-fallback",
            "false",
            "--option",
            "max-jobs",
            "1",
            "--expr",
            &expression,
        ]),
        Duration::from_secs(90),
    )
    .map_err(Error::from);
    let result = match (result, marker.remove()) {
        (Ok(output), Ok(())) => output,
        (Err(error), Ok(())) => return Err(error),
        (result, Err(cleanup)) => {
            return Err(Error::Cleanup {
                primary: result.err().map(Box::new),
                cleanup: Box::new(cleanup),
            });
        }
    };
    Ok(Observation {
        exit: result.status.code(),
        success: result.status.success(),
        stdout: result.stdout,
        stderr: result.stderr,
    })
}
pub fn host(
    runtime: &Path,
    nix: &Path,
    program: &Program,
    system: System,
) -> Result<Observation, Error> {
    if !runtime.is_absolute() || !nix.is_absolute() {
        return Err(Error::Arguments);
    }
    let result = command::capture(
        Command::new(runtime)
            .args(["exec", "--interactive", "wrix-builder"])
            .arg(&program.0)
            .args(["operator-probe", "strict-guest", "--nix"])
            .arg(nix)
            .arg("--program")
            .arg(&program.0)
            .args([
                "--marker",
                "/run/wrix-npfo4-isolation-marker",
                "--system",
                system.wire(),
                "--run",
            ]),
        Duration::from_secs(90),
    )?;
    Ok(Observation {
        exit: result.status.code(),
        success: result.status.success(),
        stdout: result.stdout,
        stderr: result.stderr,
    })
}
#[cfg(test)]
mod test;
