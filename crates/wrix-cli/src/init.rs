use std::{
    env, fmt, fs, io,
    io::Write,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode, Stdio},
};

use serde_json::Value;

pub fn run(
    profile_config_path: Option<&Path>,
    args: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    if args.first().is_some_and(|arg| is_help(arg)) {
        write_help(stdout)?;
        return Ok(ExitCode::SUCCESS);
    }

    match build_plan(profile_config_path, args) {
        Ok(plan) => {
            plan.write(stdout)?;
            Ok(ExitCode::SUCCESS)
        }
        Err(error) => {
            writeln!(stderr, "wrix init: {error}")?;
            write_help(stderr)?;
            Ok(ExitCode::FAILURE)
        }
    }
}

pub fn write_help(stdout: &mut impl Write) -> io::Result<()> {
    stdout.write_all(
        b"Initialize repository Git policy.\n\nUsage: wrix init [--deploy] [--key <name>] [--remote <name>] [--offline] [--no-sign] [--no-hooks] [--force]\n\nOptions:\n  --deploy\n  --key <name>\n  --remote <name>\n  --offline\n  --no-sign\n  --no-hooks\n  --force\n",
    )
}

fn is_help(arg: &str) -> bool {
    matches!(arg, "--help" | "-h" | "help")
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct KeyName(String);

impl KeyName {
    fn parse(value: &str, origin: &'static str) -> Result<Self, Error> {
        let value = value.trim();
        if value.is_empty()
            || value.contains('/')
            || value.contains('\\')
            || value.chars().any(char::is_whitespace)
        {
            return Err(Error::InvalidKeyName {
                origin,
                value: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for KeyName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RemoteName(String);

impl RemoteName {
    fn parse(value: &str, origin: &'static str) -> Result<Self, Error> {
        let value = value.trim();
        if value.is_empty() || value.chars().any(char::is_whitespace) {
            return Err(Error::InvalidRemoteName {
                origin,
                value: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RemoteName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum DeployPolicy {
    #[default]
    Skip,
    Provision,
}

impl DeployPolicy {
    const fn enabled(self) -> bool {
        matches!(self, Self::Provision)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SigningPolicy {
    Enabled,
    Disabled,
}

impl SigningPolicy {
    const fn from_bool(value: bool) -> Self {
        if value { Self::Enabled } else { Self::Disabled }
    }

    const fn as_bool(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HookPolicy {
    Enabled,
    Disabled,
}

impl HookPolicy {
    const fn from_bool(value: bool) -> Self {
        if value { Self::Enabled } else { Self::Disabled }
    }

    const fn as_bool(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VerificationPolicy {
    Online,
    Offline,
}

impl VerificationPolicy {
    const fn from_bool(value: bool) -> Self {
        if value { Self::Online } else { Self::Offline }
    }

    const fn as_bool(self) -> bool {
        matches!(self, Self::Online)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ForcePolicy {
    #[default]
    Keep,
    Replace,
}

impl ForcePolicy {
    const fn enabled(self) -> bool {
        matches!(self, Self::Replace)
    }
}

#[derive(Default)]
struct Flags {
    deploy: DeployPolicy,
    key_name: Option<KeyName>,
    remote: Option<RemoteName>,
    signing: Option<SigningPolicy>,
    hooks: Option<HookPolicy>,
    verification: Option<VerificationPolicy>,
    force: ForcePolicy,
}

#[derive(Default)]
struct FilePolicy {
    deploy_key: Option<KeyName>,
    signing: Option<SigningPolicy>,
    remote: Option<RemoteName>,
    hooks: Option<HookPolicy>,
    verification: Option<VerificationPolicy>,
}

#[derive(Default)]
struct ProfilePolicy {
    deploy_key: Option<KeyName>,
}

struct Plan {
    root: PathBuf,
    key_name: KeyName,
    signing: SigningPolicy,
    remote: RemoteName,
    hooks: HookPolicy,
    verification: VerificationPolicy,
    deploy: DeployPolicy,
    force: ForcePolicy,
}

impl Plan {
    fn write(&self, stdout: &mut impl Write) -> io::Result<()> {
        writeln!(stdout, "wrix init: repository policy resolved")?;
        writeln!(stdout, "repo: {}", self.root.display())?;
        writeln!(stdout, "deploy_key: {}", self.key_name)?;
        writeln!(stdout, "sign_commits: {}", self.signing.as_bool())?;
        writeln!(stdout, "remote: {}", self.remote)?;
        writeln!(stdout, "prek_hooks: {}", self.hooks.as_bool())?;
        writeln!(stdout, "online_verify: {}", self.verification.as_bool())?;
        writeln!(stdout, "deploy: {}", self.deploy.enabled())?;
        writeln!(stdout, "force: {}", self.force.enabled())
    }
}

fn build_plan(profile_config_path: Option<&Path>, args: &[String]) -> Result<Plan, Error> {
    let flags = parse_flags(args)?;
    if flags.deploy.enabled() && flags.verification == Some(VerificationPolicy::Offline) {
        return Err(Error::DeployOfflineFlag);
    }

    let current_dir = env::current_dir().map_err(Error::CurrentDir)?;
    let root = git_root(&current_dir)?;
    let file_policy = load_file_policy(&root.join("wrix.toml"))?;
    let profile_policy = load_profile_policy(profile_config_path)?;

    let key_name = flags
        .key_name
        .or(file_policy.deploy_key)
        .or(profile_policy.deploy_key)
        .map_or_else(|| derive_key_name(&root), Ok)?;
    let signing = flags
        .signing
        .or(file_policy.signing)
        .unwrap_or(SigningPolicy::Enabled);
    let remote = flags
        .remote
        .or(file_policy.remote)
        .unwrap_or_else(|| RemoteName(String::from("origin")));
    let hooks = flags
        .hooks
        .or(file_policy.hooks)
        .unwrap_or_else(|| HookPolicy::from_bool(root.join(".pre-commit-config.yaml").is_file()));
    let verification = flags
        .verification
        .or(file_policy.verification)
        .unwrap_or(VerificationPolicy::Online);

    if flags.deploy.enabled() && verification == VerificationPolicy::Offline {
        return Err(Error::DeployOfflinePolicy);
    }
    ensure_remote_exists(&root, &remote)?;

    Ok(Plan {
        root,
        key_name,
        signing,
        remote,
        hooks,
        verification,
        deploy: flags.deploy,
        force: flags.force,
    })
}

fn parse_flags(args: &[String]) -> Result<Flags, Error> {
    let mut flags = Flags::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--deploy" => {
                flags.deploy = DeployPolicy::Provision;
                index += 1;
            }
            "--key" => {
                let value = args.get(index + 1).ok_or(Error::MissingFlagValue {
                    flag: "--key",
                    value_name: "<name>",
                })?;
                flags.key_name = Some(KeyName::parse(value, "--key")?);
                index += 2;
            }
            value if value.starts_with("--key=") => {
                let value = value.trim_start_matches("--key=");
                flags.key_name = Some(KeyName::parse(value, "--key")?);
                index += 1;
            }
            "--remote" => {
                let value = args.get(index + 1).ok_or(Error::MissingFlagValue {
                    flag: "--remote",
                    value_name: "<name>",
                })?;
                flags.remote = Some(RemoteName::parse(value, "--remote")?);
                index += 2;
            }
            value if value.starts_with("--remote=") => {
                let value = value.trim_start_matches("--remote=");
                flags.remote = Some(RemoteName::parse(value, "--remote")?);
                index += 1;
            }
            "--offline" => {
                flags.verification = Some(VerificationPolicy::Offline);
                index += 1;
            }
            "--no-sign" => {
                flags.signing = Some(SigningPolicy::Disabled);
                index += 1;
            }
            "--no-hooks" => {
                flags.hooks = Some(HookPolicy::Disabled);
                index += 1;
            }
            "--force" => {
                flags.force = ForcePolicy::Replace;
                index += 1;
            }
            "--" => {
                if index + 1 == args.len() {
                    return Ok(flags);
                }
                return Err(Error::UnexpectedArgument {
                    value: args[index + 1].clone(),
                });
            }
            value => {
                return Err(Error::UnexpectedArgument {
                    value: value.to_owned(),
                });
            }
        }
    }
    Ok(flags)
}

fn git_root(current_dir: &Path) -> Result<PathBuf, Error> {
    let output = ProcessCommand::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .current_dir(current_dir)
        .stdin(Stdio::null())
        .output()
        .map_err(Error::GitIo)?;
    if !output.status.success() {
        return Err(Error::MissingGitRoot {
            cwd: path_string(current_dir),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if root.is_empty() {
        return Err(Error::MissingGitRoot {
            cwd: path_string(current_dir),
            detail: String::from("git returned an empty repository root"),
        });
    }
    Ok(PathBuf::from(root))
}

fn ensure_remote_exists(root: &Path, remote: &RemoteName) -> Result<(), Error> {
    let key = format!("remote.{}.url", remote.as_str());
    let output = ProcessCommand::new("git")
        .arg("config")
        .arg("--get")
        .arg(&key)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(Error::GitIo)?;
    if output.status.success() && !String::from_utf8_lossy(&output.stdout).trim().is_empty() {
        return Ok(());
    }
    Err(Error::MissingRemote {
        remote: remote.clone(),
    })
}

fn derive_key_name(root: &Path) -> Result<KeyName, Error> {
    let repo = root
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| Error::CannotDeriveKeyName {
            root: path_string(root),
        })?;
    let hostname = hostname()?;
    let key = format!("{repo}-{hostname}");
    KeyName::parse(&key, "derived default")
}

fn hostname() -> Result<String, Error> {
    if let Ok(value) = env::var("HOSTNAME") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_owned());
        }
    }
    let output = ProcessCommand::new("hostname")
        .stdin(Stdio::null())
        .output()
        .map_err(Error::HostnameIo)?;
    if !output.status.success() {
        return Err(Error::HostnameCommand {
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    let hostname = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if hostname.is_empty() {
        return Err(Error::HostnameCommand {
            detail: String::from("hostname returned an empty value"),
        });
    }
    Ok(hostname)
}

fn load_profile_policy(path: Option<&Path>) -> Result<ProfilePolicy, Error> {
    let Some(path) = path else {
        return Ok(ProfilePolicy::default());
    };
    let content = fs::read_to_string(path).map_err(|source| Error::ProfileConfigIo {
        path: path_string(path),
        source,
    })?;
    let value =
        serde_json::from_str::<Value>(&content).map_err(|source| Error::ProfileConfigJson {
            path: path_string(path),
            source,
        })?;
    let deploy_key = value
        .get("security")
        .and_then(Value::as_object)
        .and_then(|security| security.get("deploy_key"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|value| KeyName::parse(value, "ProfileConfig security.deploy_key"))
        .transpose()?;
    Ok(ProfilePolicy { deploy_key })
}

fn load_file_policy(path: &Path) -> Result<FilePolicy, Error> {
    if !path.exists() {
        return Ok(FilePolicy::default());
    }
    let content = fs::read_to_string(path).map_err(|source| Error::ConfigIo {
        path: path_string(path),
        source,
    })?;
    parse_file_policy(path, &content)
}

fn parse_file_policy(path: &Path, content: &str) -> Result<FilePolicy, Error> {
    let mut policy = FilePolicy::default();
    let mut section = Vec::new();
    for (line_index, line) in content.lines().enumerate() {
        let line_number = line_index + 1;
        let line = strip_comment(line).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            section = parse_section(path, line_number, line)?;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(Error::ConfigSyntax {
                path: path_string(path),
                line: line_number,
                message: String::from("expected key = value"),
            });
        };
        let mut parts = section.clone();
        parts.extend(key.trim().split('.').map(str::trim).map(ToOwned::to_owned));
        apply_file_value(path, line_number, &parts, value.trim(), &mut policy)?;
    }
    Ok(policy)
}

fn parse_section(path: &Path, line: usize, input: &str) -> Result<Vec<String>, Error> {
    if input.starts_with("[[") || !input.ends_with(']') {
        return Err(Error::ConfigSyntax {
            path: path_string(path),
            line,
            message: String::from("expected a table header like [wrix.git]"),
        });
    }
    let inner = &input[1..input.len() - 1];
    let parts = inner
        .split('.')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return Err(Error::ConfigSyntax {
            path: path_string(path),
            line,
            message: String::from("empty table header"),
        });
    }
    Ok(parts)
}

fn apply_file_value(
    path: &Path,
    line: usize,
    parts: &[String],
    value: &str,
    policy: &mut FilePolicy,
) -> Result<(), Error> {
    let parts = parts.iter().map(String::as_str).collect::<Vec<_>>();
    match parts.as_slice() {
        ["wrix", "git", "deploy_key"] => {
            let value = parse_string_value(path, line, value)?;
            policy.deploy_key = Some(KeyName::parse(&value, "wrix.git.deploy_key")?);
        }
        ["wrix", "git", "sign_commits"] => {
            policy.signing = Some(SigningPolicy::from_bool(parse_bool_value(
                path, line, value,
            )?));
        }
        ["wrix", "git", "remote"] => {
            let value = parse_string_value(path, line, value)?;
            policy.remote = Some(RemoteName::parse(&value, "wrix.git.remote")?);
        }
        ["wrix", "init", "prek_hooks"] => {
            policy.hooks = Some(HookPolicy::from_bool(parse_bool_value(path, line, value)?));
        }
        ["wrix", "init", "online_verify"] => {
            policy.verification = Some(VerificationPolicy::from_bool(parse_bool_value(
                path, line, value,
            )?));
        }
        ["wrix", ..] => {
            return Err(Error::ConfigSyntax {
                path: path_string(path),
                line,
                message: format!("unsupported wrix.toml key: {}", parts.join(".")),
            });
        }
        _ => {}
    }
    Ok(())
}

fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '#' if !in_string => return &line[..index],
            _ => {}
        }
    }
    line
}

fn parse_string_value(path: &Path, line: usize, value: &str) -> Result<String, Error> {
    let mut chars = value.chars();
    if chars.next() != Some('"') {
        return Err(Error::ConfigSyntax {
            path: path_string(path),
            line,
            message: String::from("expected a quoted string"),
        });
    }

    let mut output = String::new();
    let mut escaped = false;
    let mut closed = false;
    let mut consumed = 1;
    for character in value[1..].chars() {
        consumed += character.len_utf8();
        if escaped {
            match character {
                '"' => output.push('"'),
                '\\' => output.push('\\'),
                'n' => output.push('\n'),
                'r' => output.push('\r'),
                't' => output.push('\t'),
                other => {
                    return Err(Error::ConfigSyntax {
                        path: path_string(path),
                        line,
                        message: format!("unsupported string escape: \\{other}"),
                    });
                }
            }
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '"' => {
                closed = true;
                break;
            }
            other => output.push(other),
        }
    }
    if !closed {
        return Err(Error::ConfigSyntax {
            path: path_string(path),
            line,
            message: String::from("unterminated string"),
        });
    }
    if !value[consumed..].trim().is_empty() {
        return Err(Error::ConfigSyntax {
            path: path_string(path),
            line,
            message: String::from("unexpected content after string value"),
        });
    }
    Ok(output)
}

fn parse_bool_value(path: &Path, line: usize, value: &str) -> Result<bool, Error> {
    match value.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(Error::ConfigSyntax {
            path: path_string(path),
            line,
            message: String::from("expected true or false"),
        }),
    }
}

fn path_string(path: &Path) -> String {
    path.display().to_string()
}

#[expect(
    clippy::doc_markdown,
    reason = "displaydoc comments are user-facing CLI errors and must not add Markdown backticks"
)]
#[derive(Debug, displaydoc::Display, thiserror::Error)]
enum Error {
    /// {flag} requires {value_name}
    MissingFlagValue {
        flag: &'static str,
        value_name: &'static str,
    },
    /// unexpected wrix init argument: {value}
    UnexpectedArgument { value: String },
    /// {origin} must be a non-empty key name without whitespace or path separators: {value}
    InvalidKeyName { origin: &'static str, value: String },
    /// {origin} must be a non-empty Git remote name without whitespace: {value}
    InvalidRemoteName { origin: &'static str, value: String },
    /// --deploy cannot be used with --offline because deploy provisioning requires online verification
    DeployOfflineFlag,
    /// --deploy requires online verification; remove --deploy or set wrix.init.online_verify = true
    DeployOfflinePolicy,
    /// cannot resolve current directory: {0}
    CurrentDir(#[source] io::Error),
    /// failed to run git: {0}
    GitIo(#[source] io::Error),
    /// cannot resolve a Git worktree from {cwd}: {detail}
    MissingGitRoot { cwd: String, detail: String },
    /// configured Git remote '{remote}' is not set
    MissingRemote { remote: RemoteName },
    /// cannot derive default deploy key name from repository root {root}
    CannotDeriveKeyName { root: String },
    /// failed to resolve hostname: {0}
    HostnameIo(#[source] io::Error),
    /// failed to resolve hostname: {detail}
    HostnameCommand { detail: String },
    /// cannot read profile config {path}: {source}
    ProfileConfigIo { path: String, source: io::Error },
    /// invalid profile config JSON {path}: {source}
    ProfileConfigJson {
        path: String,
        source: serde_json::Error,
    },
    /// cannot read {path}: {source}
    ConfigIo { path: String, source: io::Error },
    /// invalid {path} at line {line}: {message}
    ConfigSyntax {
        path: String,
        line: usize,
        message: String,
    },
}

#[cfg(test)]
mod test {
    use std::path::Path;

    use super::{
        DeployPolicy, FilePolicy, ForcePolicy, HookPolicy, KeyName, RemoteName, SigningPolicy,
        VerificationPolicy, parse_file_policy, parse_flags,
    };

    #[test]
    fn flags_parse_documented_init_options() {
        let args = vec![
            String::from("--deploy"),
            String::from("--key"),
            String::from("repo-key"),
            String::from("--remote=upstream"),
            String::from("--offline"),
            String::from("--no-sign"),
            String::from("--no-hooks"),
            String::from("--force"),
        ];
        let flags = parse_flags(&args).unwrap();
        assert_eq!(flags.deploy, DeployPolicy::Provision);
        assert_eq!(flags.key_name, Some(KeyName(String::from("repo-key"))));
        assert_eq!(flags.remote, Some(RemoteName(String::from("upstream"))));
        assert_eq!(flags.verification, Some(VerificationPolicy::Offline));
        assert_eq!(flags.signing, Some(SigningPolicy::Disabled));
        assert_eq!(flags.hooks, Some(HookPolicy::Disabled));
        assert_eq!(flags.force, ForcePolicy::Replace);
    }

    #[test]
    fn wrix_toml_parses_supported_policy_keys() {
        let content = r#"
[wrix.git]
deploy_key = "toml-key"
sign_commits = false
remote = "upstream"

[wrix.init]
prek_hooks = false
online_verify = false
"#;
        let policy = parse_file_policy(Path::new("wrix.toml"), content).unwrap();
        assert_policy(
            &policy,
            Some(&KeyName(String::from("toml-key"))),
            Some(SigningPolicy::Disabled),
            Some(&RemoteName(String::from("upstream"))),
            Some(HookPolicy::Disabled),
            Some(VerificationPolicy::Offline),
        );
    }

    fn assert_policy(
        policy: &FilePolicy,
        deploy_key: Option<&KeyName>,
        signing: Option<SigningPolicy>,
        remote: Option<&RemoteName>,
        hooks: Option<HookPolicy>,
        verification: Option<VerificationPolicy>,
    ) {
        assert_eq!(policy.deploy_key.as_ref(), deploy_key);
        assert_eq!(policy.signing, signing);
        assert_eq!(policy.remote.as_ref(), remote);
        assert_eq!(policy.hooks, hooks);
        assert_eq!(policy.verification, verification);
    }
}
