use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};

#[cfg(unix)]
use std::os::unix::{fs::PermissionsExt, process::CommandExt};

use displaydoc::Display;
use serde::Deserialize;
use thiserror::Error as ThisError;
use wrix_core::path::{WorkspaceHash, WorkspaceHashParseError};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// workspace admission failed: {0}
    Admission(#[from] wrix_state::store::Error),
    /// cache helper I/O failed: {source}
    Io { source: io::Error },
    /// {source}
    Publisher { source: crate::publisher::Error },
    /// {source}
    WorkspaceHash { source: WorkspaceHashParseError },
    /// environment variable {name} is required
    MissingEnvironment { name: &'static str },
    /// environment variable {name} must be valid Unicode
    InvalidUnicodeEnvironment { name: &'static str },
    /// expected argument {name}
    ExpectedArgument { name: String },
    /// missing value for argument {name}
    MissingArgumentValue { name: String },
    /// unexpected argument {value}
    UnexpectedArgument { value: String },
    /// invalid {label} {value}
    InvalidNumber { label: &'static str, value: String },
    /// {label} must be an absolute directory: {path}
    InvalidDirectory { label: &'static str, path: String },
    /// {label} must be an absolute file: {path}
    InvalidFile { label: &'static str, path: String },
    /// {label} is not executable: {path}
    NotExecutable { label: &'static str, path: String },
    /// hook is running as uid {actual}, expected root or workspace owner {expected}
    UnexpectedUser { actual: u32, expected: u32 },
    /// command failed: {program}: {stderr}
    ProcessFailed {
        program: &'static str,
        stderr: String,
    },
    /// invalid publish manifest JSON: {source}
    ManifestJson { source: serde_json::Error },
    /// {0}
    Config(#[from] crate::config::ReadError),
    /// {0}
    Duration(#[from] wrix_core::duration::ParseError),
}

impl From<io::Error> for Error {
    fn from(source: io::Error) -> Self {
        Self::Io { source }
    }
}

impl From<crate::publisher::Error> for Error {
    fn from(source: crate::publisher::Error) -> Self {
        Self::Publisher { source }
    }
}

impl From<WorkspaceHashParseError> for Error {
    fn from(source: WorkspaceHashParseError) -> Self {
        Self::WorkspaceHash { source }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Helper {
    Hook,
    Publish,
}

#[derive(Clone, Debug)]
struct HookConfig {
    workspace_hash: WorkspaceHash,
    owner_uid: u32,
    owner_gid: u32,
    state_root: PathBuf,
    cache_root: PathBuf,
    manifest: PathBuf,
    publisher_helper: PathBuf,
    settings: PathBuf,
}

#[derive(Clone, Debug)]
struct PublishConfig {
    workspace_hash: WorkspaceHash,
    state_root: PathBuf,
    cache_root: PathBuf,
    manifest: PathBuf,
    drv_path: String,
    out_paths: String,
    settings: PathBuf,
}

#[derive(Deserialize)]
struct Manifest {
    roots: Vec<ManifestRoot>,
}

#[derive(Deserialize)]
struct ManifestRoot {
    #[serde(alias = "drvPath")]
    drv_path: String,
}

impl Helper {
    const fn binary_name(self) -> &'static str {
        match self {
            Self::Hook => "wrix-cache-hook",
            Self::Publish => "wrix-cache-publish",
        }
    }

    const fn purpose(self) -> &'static str {
        match self {
            Self::Hook => "Run the project cache post-build hook.",
            Self::Publish => "Publish project cache paths.",
        }
    }
}

pub fn main(helper: Helper) -> ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr();
    if let Err(error) = tracing_subscriber::fmt().with_writer(io::stderr).try_init() {
        if writeln!(
            stderr,
            "{}: logging initialization failed: {error}",
            helper.binary_name()
        )
        .is_err()
        {
            return ExitCode::FAILURE;
        }
        return ExitCode::FAILURE;
    }
    match run(helper, &args, &mut stdout, &mut stderr) {
        Ok(code) => code,
        Err(error) => {
            if writeln!(stderr, "{}: {error}", helper.binary_name()).is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::FAILURE
        }
    }
}

fn run(
    helper: Helper,
    args: &[String],
    stdout: &mut impl Write,
    _stderr: &mut impl Write,
) -> Result<ExitCode> {
    if args.is_empty()
        || args
            .first()
            .is_some_and(|arg| arg == "--help" || arg == "-h")
    {
        write_help(helper, stdout)?;
        return Ok(ExitCode::SUCCESS);
    }
    if helper == Helper::Publish && args.first().is_some_and(|arg| arg == "--duration-seconds") {
        let [_, input] = args else {
            return Err(Error::ExpectedArgument {
                name: "--duration-seconds <duration>".to_owned(),
            });
        };
        writeln!(stdout, "{}", wrix_core::duration::seconds(input)?)?;
        return Ok(ExitCode::SUCCESS);
    }
    match helper {
        Helper::Hook => run_hook(args, stdout),
        Helper::Publish => run_publish(args, stdout),
    }
}

fn run_hook(args: &[String], stdout: &mut impl Write) -> Result<ExitCode> {
    let config = HookConfig::parse(args)?;
    config.validate()?;
    let drv_path = required_environment("DRV_PATH")?;
    let out_paths = required_environment("OUT_PATHS")?;
    if !wrix_state::namespace::is_host_root(&config.state_root)?
        && !manifest_allows_drv(&fs::read_to_string(&config.manifest)?, &drv_path)?
    {
        writeln!(
            stdout,
            "wrix-cache-hook: skipping non-project derivation {drv_path}"
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    exec_publisher(config, drv_path, out_paths)
}

fn run_publish(args: &[String], stdout: &mut impl Write) -> Result<ExitCode> {
    let config = PublishConfig::parse(args)?;
    let settings = crate::config::Settings::read_snapshot(&config.settings)?;
    let report = crate::publisher::run_hook_record(
        crate::publisher::HookRecord {
            workspace_hash: &config.workspace_hash,
            state_root: &config.state_root,
            cache_root: &config.cache_root,
            manifest: &config.manifest,
            drv_path: &config.drv_path,
            out_paths: &config.out_paths,
        },
        settings,
    )?;
    for line in report.lines() {
        writeln!(stdout, "{line}")?;
    }
    Ok(ExitCode::SUCCESS)
}

fn exec_publisher(config: HookConfig, drv_path: String, out_paths: String) -> Result<ExitCode> {
    let current_uid = rustix::process::geteuid().as_raw();
    if current_uid != 0 && current_uid != config.owner_uid {
        return Err(Error::UnexpectedUser {
            actual: current_uid,
            expected: config.owner_uid,
        });
    }
    let mut command = Command::new(&config.publisher_helper);
    command
        .arg("--workspace-hash")
        .arg(config.workspace_hash.as_str())
        .arg("--state-root")
        .arg(config.state_root)
        .arg("--cache-root")
        .arg(config.cache_root)
        .arg("--manifest")
        .arg(config.manifest)
        .arg("--drv-path")
        .arg(drv_path)
        .arg("--out-paths")
        .arg(out_paths)
        .arg("--config")
        .arg(config.settings)
        .stdin(Stdio::null());
    #[cfg(unix)]
    if current_uid == 0 {
        command.gid(config.owner_gid).uid(config.owner_uid);
    }
    exec_command(command)
}

fn exec_command(mut command: Command) -> Result<ExitCode> {
    #[cfg(unix)]
    {
        let error = command.exec();
        Err(error.into())
    }
    #[cfg(not(unix))]
    {
        let status = command.status()?;
        Ok(if status.success() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        })
    }
}

impl HookConfig {
    fn parse(args: &[String]) -> Result<Self> {
        let mut parser = FlagParser::new(args);
        let workspace_hash = WorkspaceHash::parse(&parser.required("--workspace-hash")?)?;
        let owner_user_id = parse_u32(&parser.required("--owner-uid")?, "owner uid")?;
        let owner_group_id = parse_u32(&parser.required("--owner-gid")?, "owner gid")?;
        let state_root = PathBuf::from(parser.required("--state-root")?);
        let cache_root = PathBuf::from(parser.required("--cache-root")?);
        let manifest = PathBuf::from(parser.required("--manifest")?);
        let publisher_helper = PathBuf::from(parser.required("--publisher-helper")?);
        let settings = PathBuf::from(parser.required("--config")?);
        parser.finish()?;
        Ok(Self {
            workspace_hash,
            owner_uid: owner_user_id,
            owner_gid: owner_group_id,
            state_root,
            cache_root,
            manifest,
            publisher_helper,
            settings,
        })
    }

    fn validate(&self) -> Result<()> {
        require_absolute_dir("state root", &self.state_root)?;
        if !self.cache_root.is_absolute() {
            return Err(Error::InvalidDirectory {
                label: "cache root",
                path: self.cache_root.display().to_string(),
            });
        }
        require_absolute_file("publish manifest", &self.manifest)?;
        require_absolute_file("cache configuration snapshot", &self.settings)?;
        require_executable_file("publisher helper", &self.publisher_helper)
    }
}

impl PublishConfig {
    fn parse(args: &[String]) -> Result<Self> {
        let mut parser = FlagParser::new(args);
        let workspace_hash = WorkspaceHash::parse(&parser.required("--workspace-hash")?)?;
        let state_root = PathBuf::from(parser.required("--state-root")?);
        let cache_root = PathBuf::from(parser.required("--cache-root")?);
        let manifest = PathBuf::from(parser.required("--manifest")?);
        let drv_path = parser.required("--drv-path")?;
        let out_paths = parser.required("--out-paths")?;
        let settings = PathBuf::from(parser.required("--config")?);
        parser.finish()?;
        Ok(Self {
            workspace_hash,
            state_root,
            cache_root,
            manifest,
            drv_path,
            out_paths,
            settings,
        })
    }
}

struct FlagParser<'a> {
    args: &'a [String],
    index: usize,
}

impl<'a> FlagParser<'a> {
    const fn new(args: &'a [String]) -> Self {
        Self { args, index: 0 }
    }

    fn required(&mut self, name: &str) -> Result<String> {
        if self.args.get(self.index).map(String::as_str) != Some(name) {
            return Err(Error::ExpectedArgument {
                name: name.to_owned(),
            });
        }
        self.index += 1;
        let value =
            self.args
                .get(self.index)
                .cloned()
                .ok_or_else(|| Error::MissingArgumentValue {
                    name: name.to_owned(),
                })?;
        self.index += 1;
        Ok(value)
    }

    fn finish(&self) -> Result<()> {
        if self.index == self.args.len() {
            return Ok(());
        }
        Err(Error::UnexpectedArgument {
            value: self.args[self.index].clone(),
        })
    }
}

fn require_absolute_dir(label: &'static str, path: &Path) -> Result<()> {
    if path.is_absolute() && path.is_dir() {
        return Ok(());
    }
    Err(Error::InvalidDirectory {
        label,
        path: path.display().to_string(),
    })
}

fn require_absolute_file(label: &'static str, path: &Path) -> Result<()> {
    if path.is_absolute() && path.is_file() {
        return Ok(());
    }
    Err(Error::InvalidFile {
        label,
        path: path.display().to_string(),
    })
}

fn require_executable_file(label: &'static str, path: &Path) -> Result<()> {
    require_absolute_file(label, path)?;
    #[cfg(unix)]
    {
        let mode = fs::metadata(path)?.permissions().mode();
        if mode & 0o111 == 0 {
            return Err(Error::NotExecutable {
                label,
                path: path.display().to_string(),
            });
        }
    }
    Ok(())
}

fn manifest_allows_drv(manifest: &str, drv_path: &str) -> Result<bool> {
    let manifest = serde_json::from_str::<Manifest>(manifest)
        .map_err(|source| Error::ManifestJson { source })?;
    Ok(manifest.roots.iter().any(|root| root.drv_path == drv_path))
}

fn parse_u32(input: &str, label: &'static str) -> Result<u32> {
    input
        .parse::<u32>()
        .map_err(|_source| Error::InvalidNumber {
            label,
            value: input.to_owned(),
        })
}

fn required_environment(name: &'static str) -> Result<String> {
    match env::var(name) {
        Ok(value) => Ok(value),
        Err(env::VarError::NotPresent) => Err(Error::MissingEnvironment { name }),
        Err(env::VarError::NotUnicode(_)) => Err(Error::InvalidUnicodeEnvironment { name }),
    }
}

fn write_help(helper: Helper, stdout: &mut impl Write) -> Result<()> {
    match helper {
        Helper::Hook => writeln!(
            stdout,
            "{}\n\nUsage: {} --workspace-hash HASH --owner-uid UID --owner-gid GID --state-root PATH --cache-root PATH --manifest PATH --publisher-helper PATH --config PATH",
            helper.purpose(),
            helper.binary_name()
        ),
        Helper::Publish => writeln!(
            stdout,
            "{}\n\nUsage: {} --workspace-hash HASH --state-root PATH --cache-root PATH --manifest PATH --drv-path PATH --out-paths PATHS --config PATH",
            helper.purpose(),
            helper.binary_name()
        ),
    }?;
    Ok(())
}

#[cfg(test)]
mod test {
    use super::manifest_allows_drv;

    #[test]
    fn duration_normalization_uses_exact_seconds_without_workspace_state() {
        for (input, expected) in [("1week 2days", "777600\n"), ("1500ms 500ms", "2\n")] {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let args = ["--duration-seconds".to_owned(), input.to_owned()];
            assert_eq!(
                super::run(super::Helper::Publish, &args, &mut stdout, &mut stderr).unwrap(),
                std::process::ExitCode::SUCCESS
            );
            assert_eq!(String::from_utf8(stdout).unwrap(), expected);
            assert_eq!(stderr, Vec::<u8>::new());
        }
        for input in ["1ms", "1G", "18446744073709551616s"] {
            let mut stdout = Vec::new();
            let args = ["--duration-seconds".to_owned(), input.to_owned()];
            assert!(
                super::run(super::Helper::Publish, &args, &mut stdout, &mut Vec::new()).is_err()
            );
            assert_eq!(stdout, Vec::<u8>::new());
        }
    }

    #[test]
    fn manifest_scope_accepts_matching_drv_path() {
        let manifest = r#"{"roots":[{"name":"pkg","drv_path":"/nix/store/aaa.drv"}]}"#;
        assert!(manifest_allows_drv(manifest, "/nix/store/aaa.drv").unwrap());
        assert!(!manifest_allows_drv(manifest, "/nix/store/bbb.drv").unwrap());
    }

    #[test]
    fn manifest_parser_rejects_malformed_json() {
        assert!(manifest_allows_drv(r#"{"roots": ["#, "/nix/store/aaa.drv").is_err());
    }
}
