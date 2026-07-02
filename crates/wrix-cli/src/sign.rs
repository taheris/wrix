use std::{
    env,
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode, ExitStatus, Stdio},
};

pub const PROGRAM_NAME: &str = "wrix-git-sign";
pub const ALLOWED_SIGNERS_CONFIG: &str = "wrix/allowed_signers";

const SIGNING_KEY_PREFIX: &str = "wrix/signing-key/";

pub fn signing_key_config(key_name: &str) -> String {
    format!("{SIGNING_KEY_PREFIX}{}", signing_key_file_name(key_name))
}

pub fn signing_key_file_name(key_name: &str) -> String {
    format!("{key_name}-signing")
}

pub fn resolve_key_for_key_name(key_name: &str) -> Result<PathBuf, Error> {
    resolve_key_file(&signing_key_file_name(key_name))
}

pub fn public_key(path: &Path) -> Result<String, Error> {
    let output = ProcessCommand::new("ssh-keygen")
        .arg("-y")
        .arg("-f")
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|source| Error::SshKeygenIo { source })?;
    if output.status.success() {
        let public_key = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !public_key.is_empty() {
            return Ok(public_key);
        }
    }
    Err(Error::SshPublicKey {
        path: path_string(path),
        detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

pub fn run_program(args: Vec<OsString>) -> Result<ExitCode, Error> {
    if args.len() == 1 && args[0] == OsStr::new("--wrix-probe") {
        return Ok(ExitCode::SUCCESS);
    }
    let args = rewrite_args(args)?;
    let status = ProcessCommand::new("ssh-keygen")
        .args(args)
        .status()
        .map_err(|source| Error::SshKeygenIo { source })?;
    Ok(exit_code(status))
}

fn rewrite_args(args: Vec<OsString>) -> Result<Vec<OsString>, Error> {
    args.into_iter().map(rewrite_arg).collect()
}

fn rewrite_arg(arg: OsString) -> Result<OsString, Error> {
    if arg == OsStr::new(ALLOWED_SIGNERS_CONFIG) {
        return Ok(common_allowed_signers()?.into_os_string());
    }
    if let Some(value) = arg.to_str()
        && let Some(key_file) = signing_key_from_config(value)?
    {
        return Ok(resolve_key_file(&key_file)?.into_os_string());
    }
    Ok(arg)
}

fn signing_key_from_config(value: &str) -> Result<Option<String>, Error> {
    let Some(key_file) = value.strip_prefix(SIGNING_KEY_PREFIX) else {
        return Ok(None);
    };
    if key_file.is_empty()
        || key_file.contains('/')
        || key_file.contains('\\')
        || key_file.chars().any(char::is_whitespace)
    {
        return Err(Error::InvalidSigningKeyToken {
            value: value.to_owned(),
        });
    }
    Ok(Some(key_file.to_owned()))
}

fn resolve_key_file(key_file: &str) -> Result<PathBuf, Error> {
    match env::var("WRIX_SIGNING_KEY") {
        Ok(value) if !value.trim().is_empty() => {
            let path = PathBuf::from(value);
            if path.is_file() {
                return path.canonicalize().map_err(|source| Error::StateIo {
                    path: path_string(&path),
                    source,
                });
            }
            return Err(Error::SigningKeyEnvMissing {
                path: path_string(&path),
            });
        }
        Ok(_) | Err(env::VarError::NotPresent) => {}
        Err(source) => return Err(Error::SigningKeyEnvInvalid { source }),
    }

    let home = env::var("HOME").map_err(|source| Error::HomeMissing { source })?;
    if home.trim().is_empty() {
        return Err(Error::HomeEmpty);
    }
    let path = Path::new(&home)
        .join(".ssh")
        .join("deploy_keys")
        .join(key_file);
    if path.is_file() {
        return path.canonicalize().map_err(|source| Error::StateIo {
            path: path_string(&path),
            source,
        });
    }
    Err(Error::SigningKeyMissing {
        path: path_string(&path),
    })
}

fn common_allowed_signers() -> Result<PathBuf, Error> {
    let common_dir = git_common_dir()?;
    Ok(common_dir.join("wrix").join("allowed_signers"))
}

fn git_common_dir() -> Result<PathBuf, Error> {
    let value = git_stdout(&["rev-parse", "--git-common-dir"])?;
    if value.is_empty() {
        return Err(Error::GitCommonDir {
            detail: String::from("git returned an empty common directory"),
        });
    }
    let common_dir = PathBuf::from(&value);
    if common_dir.is_absolute() {
        return Ok(common_dir);
    }
    let root = git_stdout(&["rev-parse", "--show-toplevel"])?;
    if root.is_empty() {
        return Err(Error::GitRoot {
            detail: String::from("git returned an empty repository root"),
        });
    }
    Ok(PathBuf::from(root).join(common_dir))
}

fn git_stdout(args: &[&str]) -> Result<String, Error> {
    let output = ProcessCommand::new("git")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|source| Error::GitIo { source })?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    Err(Error::GitCommonDir {
        detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

fn exit_code(status: ExitStatus) -> ExitCode {
    status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .map_or(ExitCode::FAILURE, ExitCode::from)
}

fn path_string(path: &Path) -> String {
    path.display().to_string()
}

#[expect(
    clippy::doc_markdown,
    reason = "displaydoc comments are user-facing helper errors and must not add Markdown backticks"
)]
#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum Error {
    /// WRIX_SIGNING_KEY is not valid Unicode: {source}
    SigningKeyEnvInvalid { source: env::VarError },
    /// WRIX_SIGNING_KEY does not point at a file: {path}
    SigningKeyEnvMissing { path: String },
    /// HOME is not set; cannot resolve fallback signing key: {source}
    HomeMissing { source: env::VarError },
    /// HOME is empty; cannot resolve fallback signing key
    HomeEmpty,
    /// fallback signing key does not exist: {path}
    SigningKeyMissing { path: String },
    /// cannot read Wrix signing state at {path}: {source}
    StateIo { path: String, source: io::Error },
    /// cannot execute ssh-keygen: {source}
    SshKeygenIo { source: io::Error },
    /// cannot derive SSH public key from signing key {path}: {detail}
    SshPublicKey { path: String, detail: String },
    /// failed to run git: {source}
    GitIo { source: io::Error },
    /// cannot resolve Git common directory: {detail}
    GitCommonDir { detail: String },
    /// cannot resolve Git worktree root: {detail}
    GitRoot { detail: String },
    /// invalid Wrix signing-key token in Git config: {value}
    InvalidSigningKeyToken { value: String },
}
