use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};

#[cfg(unix)]
use std::os::unix::{fs::PermissionsExt, process::CommandExt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Helper {
    Hook,
    Publish,
    Serve,
}

#[derive(Clone, Debug)]
struct HookConfig {
    workspace_hash: String,
    owner_uid: u32,
    owner_gid: u32,
    state_root: PathBuf,
    cache_root: PathBuf,
    manifest: PathBuf,
    publisher_helper: PathBuf,
}

#[derive(Clone, Debug)]
struct PublishConfig {
    workspace_hash: String,
    state_root: PathBuf,
    cache_root: PathBuf,
    manifest: PathBuf,
    drv_path: String,
    out_paths: String,
}

impl Helper {
    const fn binary_name(self) -> &'static str {
        match self {
            Self::Hook => "wrix-cache-hook",
            Self::Publish => "wrix-cache-publish",
            Self::Serve => "wrix-cache-serve",
        }
    }

    const fn purpose(self) -> &'static str {
        match self {
            Self::Hook => "Run the project cache post-build hook.",
            Self::Publish => "Publish project cache paths.",
            Self::Serve => "Serve the project cache over HTTP.",
        }
    }
}

pub fn main(helper: Helper) -> ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
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
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    if args.is_empty()
        || args
            .first()
            .is_some_and(|arg| arg == "--help" || arg == "-h")
    {
        write_help(helper, stdout)?;
        return Ok(ExitCode::SUCCESS);
    }
    match helper {
        Helper::Hook => run_hook(args, stdout),
        Helper::Publish => run_publish(args, stdout),
        Helper::Serve => {
            writeln!(
                stderr,
                "{} accepts no public arguments yet; pass --help for usage.",
                helper.binary_name()
            )?;
            Ok(ExitCode::FAILURE)
        }
    }
}

fn run_hook(args: &[String], stdout: &mut impl Write) -> io::Result<ExitCode> {
    let config = HookConfig::parse(args)?;
    config.validate()?;
    let drv_path = env::var("DRV_PATH").map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "DRV_PATH is required for the post-build hook",
        )
    })?;
    let out_paths = env::var("OUT_PATHS").unwrap_or_else(|_| String::new());
    let manifest = fs::read_to_string(&config.manifest)?;
    if !manifest_allows_drv(&manifest, &drv_path) {
        writeln!(
            stdout,
            "wrix-cache-hook: skipping non-project derivation {drv_path}"
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    exec_publisher(config, drv_path, out_paths)
}

fn run_publish(args: &[String], stdout: &mut impl Write) -> io::Result<ExitCode> {
    let config = PublishConfig::parse(args)?;
    writeln!(
        stdout,
        "wrix-cache-publish: accepted {} for workspace {} using {} and {} ({})",
        config.drv_path,
        config.workspace_hash,
        config.state_root.display(),
        config.cache_root.display(),
        config.manifest.display()
    )?;
    if !config.out_paths.is_empty() {
        writeln!(stdout, "out_paths: {}", config.out_paths)?;
    }
    Ok(ExitCode::SUCCESS)
}

fn exec_publisher(config: HookConfig, drv_path: String, out_paths: String) -> io::Result<ExitCode> {
    let current_uid = current_uid()?;
    if current_uid != 0 && current_uid != config.owner_uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "hook is running as uid {current_uid}, expected root or workspace owner {}",
                config.owner_uid
            ),
        ));
    }
    let mut command = Command::new(&config.publisher_helper);
    command
        .arg("--workspace-hash")
        .arg(config.workspace_hash)
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
        .stdin(Stdio::null());
    #[cfg(unix)]
    if current_uid == 0 {
        command.gid(config.owner_gid).uid(config.owner_uid);
    }
    exec_command(command)
}

fn exec_command(mut command: Command) -> io::Result<ExitCode> {
    #[cfg(unix)]
    {
        let error = command.exec();
        Err(error)
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

fn current_uid() -> io::Result<u32> {
    let output = Command::new("id")
        .arg("-u")
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    parse_u32(
        String::from_utf8_lossy(&output.stdout).trim(),
        "current uid",
    )
}

impl HookConfig {
    fn parse(args: &[String]) -> io::Result<Self> {
        let mut parser = FlagParser::new(args);
        let workspace_hash = parser.required("--workspace-hash")?;
        let owner_user_id = parse_u32(&parser.required("--owner-uid")?, "owner uid")?;
        let owner_group_id = parse_u32(&parser.required("--owner-gid")?, "owner gid")?;
        let state_root = PathBuf::from(parser.required("--state-root")?);
        let cache_root = PathBuf::from(parser.required("--cache-root")?);
        let manifest = PathBuf::from(parser.required("--manifest")?);
        let publisher_helper = PathBuf::from(parser.required("--publisher-helper")?);
        parser.finish()?;
        Ok(Self {
            workspace_hash,
            owner_uid: owner_user_id,
            owner_gid: owner_group_id,
            state_root,
            cache_root,
            manifest,
            publisher_helper,
        })
    }

    fn validate(&self) -> io::Result<()> {
        validate_workspace_hash(&self.workspace_hash)?;
        require_absolute_dir("state root", &self.state_root)?;
        require_absolute_dir("cache root", &self.cache_root)?;
        require_absolute_file("publish manifest", &self.manifest)?;
        require_executable_file("publisher helper", &self.publisher_helper)
    }
}

impl PublishConfig {
    fn parse(args: &[String]) -> io::Result<Self> {
        let mut parser = FlagParser::new(args);
        let workspace_hash = parser.required("--workspace-hash")?;
        let state_root = PathBuf::from(parser.required("--state-root")?);
        let cache_root = PathBuf::from(parser.required("--cache-root")?);
        let manifest = PathBuf::from(parser.required("--manifest")?);
        let drv_path = parser.required("--drv-path")?;
        let out_paths = parser.required("--out-paths")?;
        parser.finish()?;
        Ok(Self {
            workspace_hash,
            state_root,
            cache_root,
            manifest,
            drv_path,
            out_paths,
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

    fn required(&mut self, name: &str) -> io::Result<String> {
        if self.args.get(self.index).map(String::as_str) != Some(name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("expected {name}"),
            ));
        }
        self.index += 1;
        let value = self.args.get(self.index).cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("missing value for {name}"),
            )
        })?;
        self.index += 1;
        Ok(value)
    }

    fn finish(&self) -> io::Result<()> {
        if self.index == self.args.len() {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unexpected argument {}", self.args[self.index]),
        ))
    }
}

fn validate_workspace_hash(hash: &str) -> io::Result<()> {
    if hash.len() == 16 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "workspace hash must be sixteen hexadecimal characters",
    ))
}

fn require_absolute_dir(label: &str, path: &Path) -> io::Result<()> {
    if path.is_absolute() && path.is_dir() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{label} must be an absolute directory: {}", path.display()),
    ))
}

fn require_absolute_file(label: &str, path: &Path) -> io::Result<()> {
    if path.is_absolute() && path.is_file() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{label} must be an absolute file: {}", path.display()),
    ))
}

fn require_executable_file(label: &str, path: &Path) -> io::Result<()> {
    require_absolute_file(label, path)?;
    #[cfg(unix)]
    {
        let mode = fs::metadata(path)?.permissions().mode();
        if mode & 0o111 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{label} is not executable: {}", path.display()),
            ));
        }
    }
    Ok(())
}

fn manifest_allows_drv(manifest: &str, drv_path: &str) -> bool {
    manifest_key_values(manifest)
        .into_iter()
        .any(|(key, value)| matches!(key.as_str(), "drv_path" | "drvPath") && value == drv_path)
}

fn manifest_key_values(input: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut rest = input;
    while let Some(key_start) = rest.find('"') {
        let after_key_start = &rest[key_start + 1..];
        let Some(key_end) = after_key_start.find('"') else {
            break;
        };
        let key = &after_key_start[..key_end];
        let after_key = &after_key_start[key_end + 1..];
        let Some(colon) = after_key.find(':') else {
            rest = after_key;
            continue;
        };
        let after_colon = after_key[colon + 1..].trim_start();
        if let Some(value_start) = after_colon.strip_prefix('"')
            && let Some(value_end) = value_start.find('"')
        {
            pairs.push((key.to_owned(), value_start[..value_end].to_owned()));
            rest = &value_start[value_end + 1..];
            continue;
        }
        rest = after_colon;
    }
    pairs
}

fn parse_u32(input: &str, label: &str) -> io::Result<u32> {
    input.parse::<u32>().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid {label} {input}: {error}"),
        )
    })
}

fn write_help(helper: Helper, stdout: &mut impl Write) -> io::Result<()> {
    match helper {
        Helper::Hook => writeln!(
            stdout,
            "{}\n\nUsage: {} --workspace-hash HASH --owner-uid UID --owner-gid GID --state-root PATH --cache-root PATH --manifest PATH --publisher-helper PATH",
            helper.purpose(),
            helper.binary_name()
        ),
        Helper::Publish => writeln!(
            stdout,
            "{}\n\nUsage: {} --workspace-hash HASH --state-root PATH --cache-root PATH --manifest PATH --drv-path PATH --out-paths PATHS",
            helper.purpose(),
            helper.binary_name()
        ),
        Helper::Serve => writeln!(
            stdout,
            "{}\n\nUsage: {} [--help]",
            helper.purpose(),
            helper.binary_name()
        ),
    }
}

#[cfg(test)]
mod test {
    use super::{manifest_allows_drv, manifest_key_values};

    #[test]
    fn manifest_scope_accepts_matching_drv_path() {
        let manifest = r#"{"roots":[{"name":"pkg","drv_path":"/nix/store/aaa.drv"}]}"#;
        assert!(manifest_allows_drv(manifest, "/nix/store/aaa.drv"));
        assert!(!manifest_allows_drv(manifest, "/nix/store/bbb.drv"));
    }

    #[test]
    fn manifest_parser_extracts_string_pairs() {
        let pairs = manifest_key_values(r#"{"drvPath":"/nix/store/aaa.drv"}"#);
        assert_eq!(
            pairs,
            vec![(String::from("drvPath"), String::from("/nix/store/aaa.drv"))]
        );
    }
}
