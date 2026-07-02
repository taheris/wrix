use std::{
    env, fmt, fs, io,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode, Stdio},
};

use serde_json::Value;

const GITHUB_KNOWN_HOSTS: &str = concat!(
    "github.com ssh-ed25519 ",
    "AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl\n",
    "github.com ssh-rsa ",
    "AAAAB3NzaC1yc2EAAAADAQABAAABgQCj7ndNxQowgcQnjshcLrqPEiiphnt+VTTvDP6mHBL9j1aNUkY4Ue1gvwnGLVlOhGeYrnZaMgRK6+PKCUXaDbC7qtbW8gIkhL7aGCsOr/C56SJMy/BCZfxd1nWzAOxSDPgVsmerOBYfNqltV9/hWCqBywINIR+5dIg6JTJ72pcEpEjcYgXkE2YEFXV1JHnsKgbLWNlhScqb2UmyRkQyytRLtL+38TGxkxCflmO+5Z8CSSNY7GidjMIZ7Q4zMjA2n1nGrlTDkzwDCsw+wqFPGQA179cnfGWOWRVruj16z6XyvxvjJwbz0wQZ75XK5tKSb7FNyeIEs4TT4jk+S4dhPeAUC5y+bDYirYgM4GC7uEnztnZyaVWQ7B381AK4Qdrwt51ZqExKbQpTUNn+EjqoTwvqNj4kqx5QUCI0ThS/YkOxJCXmPUWZbhjpCg56i+2aB6CmK2JGhn57K5mj0MNdBXA4/WnwH6XoPWJzK5Nyu2zB3nAZp+S5hpQs+p1vN1/wsjk=\n",
    "github.com ecdsa-sha2-nistp256 ",
    "AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBEmKSENjQEezOmxkZMy7opKgwFB9nkt5YRrYMjNuG5N87uRgg6CLrbo5wAdT/y6v0mKV0U2w0WZ2YB/++Tpockg=\n",
);

const TRANSPORT_TRAMPOLINE: &str = concat!(
    "sh -c '",
    "common_dir=$(git rev-parse --git-common-dir) || exit 255; ",
    "case \"$common_dir\" in /*) ;; *) ",
    "root=$(git rev-parse --show-toplevel) || exit 255; ",
    "common_dir=\"$root/$common_dir\" ;; esac; ",
    "exec \"$common_dir/wrix/git-ssh\" \"$@\"' wrix-git-ssh",
);
const PREK_HOOKS_ENV: &str = "WRIX_PREK_HOOKS";
const PREK_HOOK_NAMES: [&str; 5] = [
    "pre-commit",
    "pre-push",
    "prepare-commit-msg",
    "post-checkout",
    "post-merge",
];

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

    match build_plan(profile_config_path, args).and_then(|plan| {
        plan.apply()?;
        Ok(plan)
    }) {
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
    signing_key: Option<PathBuf>,
    remote: RemoteName,
    hooks: HookPolicy,
    verification: VerificationPolicy,
    deploy: DeployPolicy,
    force: ForcePolicy,
}

struct SigningIdentity {
    name: String,
    email: String,
    principals: Vec<String>,
}

impl Plan {
    fn apply(&self) -> Result<(), Error> {
        let common_dir = git_common_dir(&self.root)?;
        let state_dir = common_dir.join("wrix");
        create_secure_dir(&state_dir)?;
        let known_hosts_path = state_dir.join("github_known_hosts");
        write_secure_file(&known_hosts_path, GITHUB_KNOWN_HOSTS, 0o600)?;
        let helper_path = state_dir.join("git-ssh");
        write_secure_file(
            &helper_path,
            &transport_helper_script(&self.key_name),
            0o700,
        )?;
        write_common_git_config(&common_dir, "core.sshCommand", TRANSPORT_TRAMPOLINE)?;
        let signing_identity = configure_signing(
            &self.root,
            &common_dir,
            &self.key_name,
            self.signing_key.as_deref(),
        )?;
        configure_prek_hooks(&self.root, &common_dir, self.hooks)?;
        verify_transport_helper(
            &self.root,
            &common_dir,
            &helper_path,
            &known_hosts_path,
            &self.key_name,
        )?;
        if let Some(identity) = signing_identity {
            verify_signing_commit(&self.root, &identity)?;
        }
        if self.verification == VerificationPolicy::Online {
            verify_online(&self.root, &common_dir, &self.remote)?;
        }
        Ok(())
    }

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
    let signing_key = if signing == SigningPolicy::Enabled {
        require_signing_program()?;
        Some(crate::sign::resolve_key_for_key_name(
            &key_name.to_string(),
        )?)
    } else {
        None
    };

    Ok(Plan {
        root,
        key_name,
        signing,
        signing_key,
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

fn git_common_dir(root: &Path) -> Result<PathBuf, Error> {
    let output = ProcessCommand::new("git")
        .arg("rev-parse")
        .arg("--git-common-dir")
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(Error::GitIo)?;
    if !output.status.success() {
        return Err(Error::GitCommonDir {
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if value.is_empty() {
        return Err(Error::GitCommonDir {
            detail: String::from("git returned an empty common directory"),
        });
    }
    let common_dir = PathBuf::from(value);
    let common_dir = if common_dir.is_absolute() {
        common_dir
    } else {
        root.join(common_dir)
    };
    common_dir.canonicalize().map_err(|source| Error::StateIo {
        path: path_string(&common_dir),
        source,
    })
}

fn create_secure_dir(path: &Path) -> Result<(), Error> {
    fs::create_dir_all(path).map_err(|source| Error::StateIo {
        path: path_string(path),
        source,
    })?;
    set_mode(path, 0o700)
}

fn write_secure_file(path: &Path, content: &str, mode: u32) -> Result<(), Error> {
    fs::write(path, content).map_err(|source| Error::StateIo {
        path: path_string(path),
        source,
    })?;
    set_mode(path, mode)
}

fn set_mode(path: &Path, mode: u32) -> Result<(), Error> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|source| {
        Error::Permission {
            path: path_string(path),
            mode: format!("{mode:04o}"),
            source,
        }
    })
}

fn write_common_git_config(common_dir: &Path, key: &str, value: &str) -> Result<(), Error> {
    let config_path = common_dir.join("config");
    let output = ProcessCommand::new("git")
        .arg("config")
        .arg("--file")
        .arg(&config_path)
        .arg("--replace-all")
        .arg(key)
        .arg(value)
        .stdin(Stdio::null())
        .output()
        .map_err(Error::GitIo)?;
    if output.status.success() {
        return Ok(());
    }
    Err(Error::GitConfig {
        key: key.to_owned(),
        detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

fn require_signing_program() -> Result<(), Error> {
    let output = ProcessCommand::new(crate::sign::PROGRAM_NAME)
        .arg("--wrix-probe")
        .stdin(Stdio::null())
        .output()
        .map_err(|source| Error::SigningProgramIo { source })?;
    if output.status.success() {
        return Ok(());
    }
    Err(Error::SigningProgramProbe {
        detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

fn configure_signing(
    root: &Path,
    common_dir: &Path,
    key_name: &KeyName,
    signing_key: Option<&Path>,
) -> Result<Option<SigningIdentity>, Error> {
    let Some(signing_key) = signing_key else {
        write_common_git_config(common_dir, "commit.gpgsign", "false")?;
        return Ok(None);
    };
    require_private_key_mode("signing key", signing_key)?;
    let identity = signing_identity(root)?;
    let public_key = crate::sign::public_key(signing_key)?;
    write_allowed_signers(common_dir, &identity.principals, &public_key)?;
    write_common_git_config(common_dir, "gpg.format", "ssh")?;
    write_common_git_config(common_dir, "gpg.ssh.program", crate::sign::PROGRAM_NAME)?;
    write_common_git_config(
        common_dir,
        "gpg.ssh.allowedSignersFile",
        crate::sign::ALLOWED_SIGNERS_CONFIG,
    )?;
    write_common_git_config(
        common_dir,
        "user.signingkey",
        &crate::sign::signing_key_config(&key_name.to_string()),
    )?;
    write_common_git_config(common_dir, "commit.gpgsign", "true")?;
    verify_signing_config(root, common_dir, key_name)?;
    Ok(Some(identity))
}

fn write_allowed_signers(
    common_dir: &Path,
    principals: &[String],
    public_key: &str,
) -> Result<(), Error> {
    let path = common_dir.join("wrix").join("allowed_signers");
    let mut content = String::new();
    for principal in principals {
        content.push_str(principal);
        content.push(' ');
        content.push_str(public_key);
        content.push('\n');
    }
    write_secure_file(&path, &content, 0o600)
}

fn verify_signing_config(root: &Path, common_dir: &Path, key_name: &KeyName) -> Result<(), Error> {
    require_common_config_value(common_dir, "gpg.format", "ssh")?;
    require_stable_common_config_value(
        root,
        common_dir,
        "gpg.ssh.program",
        crate::sign::PROGRAM_NAME,
    )?;
    require_stable_common_config_value(
        root,
        common_dir,
        "gpg.ssh.allowedSignersFile",
        crate::sign::ALLOWED_SIGNERS_CONFIG,
    )?;
    require_stable_common_config_value(
        root,
        common_dir,
        "user.signingkey",
        &crate::sign::signing_key_config(&key_name.to_string()),
    )?;
    require_common_config_value(common_dir, "commit.gpgsign", "true")?;
    require_mode(common_dir.join("wrix").join("allowed_signers"), 0o600)
}

fn require_common_config_value(common_dir: &Path, key: &str, expected: &str) -> Result<(), Error> {
    let value = read_common_git_config(common_dir, key)?;
    if value == expected {
        return Ok(());
    }
    Err(Error::SigningConfigMismatch {
        key: key.to_owned(),
        value,
        expected: expected.to_owned(),
    })
}

fn require_stable_common_config_value(
    root: &Path,
    common_dir: &Path,
    key: &str,
    expected: &str,
) -> Result<(), Error> {
    require_common_config_value(common_dir, key, expected)?;
    let value = read_common_git_config(common_dir, key)?;
    ensure_context_stable_signing_config(root, key, &value)
}

fn ensure_context_stable_signing_config(root: &Path, key: &str, value: &str) -> Result<(), Error> {
    let root = path_string(root);
    let forbidden = [
        ("/nix/store", "host-only Nix store path"),
        ("/etc/wrix/keys", "container private-key path"),
        ("/workspace", "container workspace path"),
        (".ssh/deploy_keys", "private-key lookup path"),
        ("WRIX_SIGNING_KEY", "environment private-key path"),
    ];
    if !root.is_empty() && root != "/" && value.contains(&root) {
        return Err(Error::SigningConfigUnstable {
            key: key.to_owned(),
            value: value.to_owned(),
            reason: String::from("workspace path"),
        });
    }
    for (needle, reason) in forbidden {
        if value.contains(needle) {
            return Err(Error::SigningConfigUnstable {
                key: key.to_owned(),
                value: value.to_owned(),
                reason: String::from(reason),
            });
        }
    }
    Ok(())
}

fn signing_identity(root: &Path) -> Result<SigningIdentity, Error> {
    let mut principals = Vec::new();
    if let Some(value) = optional_git_config(root, "user.email")? {
        push_principal(&mut principals, &value);
    }
    if let Some(value) = optional_env("GIT_AUTHOR_EMAIL")? {
        push_principal(&mut principals, &value);
    }
    if let Some(value) = optional_env("GIT_COMMITTER_EMAIL")? {
        push_principal(&mut principals, &value);
    }
    if principals.is_empty() {
        principals.push(String::from("sandbox@wrix.dev"));
    }
    let email = principals
        .first()
        .cloned()
        .unwrap_or_else(|| String::from("sandbox@wrix.dev"));
    Ok(SigningIdentity {
        name: signing_name(root)?,
        email,
        principals,
    })
}

fn signing_name(root: &Path) -> Result<String, Error> {
    if let Some(value) = optional_env("GIT_COMMITTER_NAME")? {
        return Ok(value);
    }
    if let Some(value) = optional_env("GIT_AUTHOR_NAME")? {
        return Ok(value);
    }
    if let Some(value) = optional_git_config(root, "user.name")? {
        return Ok(value);
    }
    Ok(String::from("Wrix Init Verification"))
}

fn push_principal(principals: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return;
    }
    if principals.iter().any(|existing| existing == value) {
        return;
    }
    principals.push(value.to_owned());
}

fn optional_env(name: &'static str) -> Result<Option<String>, Error> {
    match env::var(name) {
        Ok(value) => {
            let value = value.trim();
            if value.is_empty() {
                Ok(None)
            } else {
                Ok(Some(value.to_owned()))
            }
        }
        Err(env::VarError::NotPresent) => Ok(None),
        Err(source) => Err(Error::IdentityEnvInvalid { name, source }),
    }
}

fn optional_git_config(root: &Path, key: &str) -> Result<Option<String>, Error> {
    let output = ProcessCommand::new("git")
        .arg("config")
        .arg("--get")
        .arg(key)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(Error::GitIo)?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if output.status.success() {
        if value.is_empty() {
            return Ok(None);
        }
        return Ok(Some(value));
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if output.status.code() == Some(1) && detail.is_empty() {
        return Ok(None);
    }
    Err(Error::GitConfigQuery {
        key: key.to_owned(),
        detail,
    })
}

fn verify_signing_commit(root: &Path, identity: &SigningIdentity) -> Result<(), Error> {
    let tree = git_stdout(root, &["mktree"])?;
    let output = ProcessCommand::new("git")
        .arg("commit-tree")
        .arg("-S")
        .arg(&tree)
        .arg("-m")
        .arg("wrix init signing verification")
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", &identity.name)
        .env("GIT_AUTHOR_EMAIL", &identity.email)
        .env("GIT_COMMITTER_NAME", &identity.name)
        .env("GIT_COMMITTER_EMAIL", &identity.email)
        .stdin(Stdio::null())
        .output()
        .map_err(Error::GitIo)?;
    if !output.status.success() {
        return Err(Error::SigningCommit {
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if commit.is_empty() {
        return Err(Error::SigningCommit {
            detail: String::from("git commit-tree returned an empty commit id"),
        });
    }
    let output = ProcessCommand::new("git")
        .arg("verify-commit")
        .arg(&commit)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(Error::GitIo)?;
    if output.status.success() {
        return Ok(());
    }
    Err(Error::SigningVerify {
        detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

fn git_stdout(root: &Path, args: &[&str]) -> Result<String, Error> {
    let output = ProcessCommand::new("git")
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(Error::GitIo)?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    Err(Error::GitCommand {
        command: args.join(" "),
        detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

fn configure_prek_hooks(root: &Path, common_dir: &Path, policy: HookPolicy) -> Result<(), Error> {
    if policy == HookPolicy::Disabled || !root.join(".pre-commit-config.yaml").is_file() {
        return Ok(());
    }
    let hooks_path = resolve_prek_hooks_path()?;
    write_common_git_config(common_dir, "core.hooksPath", &path_string(&hooks_path))?;
    verify_prek_hooks(common_dir, &hooks_path)
}

fn resolve_prek_hooks_path() -> Result<PathBuf, Error> {
    let value = match env::var(PREK_HOOKS_ENV) {
        Ok(value) => value,
        Err(env::VarError::NotPresent) => return Err(Error::PrekHooksEnvMissing),
        Err(source) => return Err(Error::PrekHooksEnvInvalid { source }),
    };
    if value.trim().is_empty() {
        return Err(Error::PrekHooksEnvEmpty);
    }
    let path = PathBuf::from(value);
    let metadata = fs::metadata(&path).map_err(|source| Error::PrekHooksIo {
        path: path_string(&path),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(Error::PrekHooksNotDirectory {
            path: path_string(&path),
        });
    }
    path.canonicalize().map_err(|source| Error::PrekHooksIo {
        path: path_string(&path),
        source,
    })
}

fn verify_prek_hooks(common_dir: &Path, hooks_path: &Path) -> Result<(), Error> {
    let configured = read_common_git_config(common_dir, "core.hooksPath")?;
    let expected = path_string(hooks_path);
    if configured != expected {
        return Err(Error::PrekHooksConfigMismatch {
            value: configured,
            expected,
        });
    }
    for hook_name in PREK_HOOK_NAMES {
        require_executable_hook(hooks_path.join(hook_name))?;
    }
    Ok(())
}

fn require_executable_hook(path: impl AsRef<Path>) -> Result<(), Error> {
    let path = path.as_ref();
    let metadata = fs::metadata(path).map_err(|source| Error::PrekHooksIo {
        path: path_string(path),
        source,
    })?;
    let is_executable = metadata.is_file() && (metadata.permissions().mode() & 0o111) != 0;
    if is_executable {
        return Ok(());
    }
    Err(Error::PrekHookNotExecutable {
        path: path_string(path),
    })
}

fn read_common_git_config(common_dir: &Path, key: &str) -> Result<String, Error> {
    let output = ProcessCommand::new("git")
        .arg("config")
        .arg("--file")
        .arg(common_dir.join("config"))
        .arg("--get")
        .arg(key)
        .stdin(Stdio::null())
        .output()
        .map_err(Error::GitIo)?;
    if !output.status.success() {
        return Err(Error::GitConfig {
            key: key.to_owned(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn verify_transport_helper(
    root: &Path,
    common_dir: &Path,
    helper_path: &Path,
    known_hosts_path: &Path,
    key_name: &KeyName,
) -> Result<(), Error> {
    let configured = read_common_git_config(common_dir, "core.sshCommand")?;
    ensure_context_stable_ssh_command(root, &configured)?;
    if configured != TRANSPORT_TRAMPOLINE {
        return Err(Error::TransportConfigMismatch { value: configured });
    }
    require_mode(common_dir.join("wrix"), 0o700)?;
    require_mode(helper_path, 0o700)?;
    require_mode(known_hosts_path, 0o600)?;
    let expected_key = resolved_deploy_key(key_name)?;
    require_private_key_mode("deploy key", &expected_key)?;
    let effective_config = probe_helper_config(root, helper_path)?;
    require_ssh_config_value(&effective_config, "batchmode", "yes")?;
    require_ssh_config_value(&effective_config, "identitiesonly", "yes")?;
    require_ssh_config_value_any(&effective_config, "stricthostkeychecking", &["yes", "true"])?;
    require_ssh_config_value(&effective_config, "identityagent", "none")?;
    require_ssh_config_value(&effective_config, "globalknownhostsfile", "/dev/null")?;
    require_ssh_config_value(
        &effective_config,
        "userknownhostsfile",
        &path_string(known_hosts_path),
    )?;
    require_ssh_config_value(&effective_config, "identityfile", "none")?;
    require_ssh_config_value(
        &effective_config,
        "identityfile",
        &path_string(&expected_key),
    )
}

fn verify_online(root: &Path, common_dir: &Path, remote: &RemoteName) -> Result<(), Error> {
    let cwd = online_verification_cwd(root, common_dir)?;
    require_runtime_git_config(&cwd, "core.sshCommand", TRANSPORT_TRAMPOLINE)?;
    let output = online_git_ls_remote(&cwd, remote)?;
    if output.status.success() {
        return Ok(());
    }
    let detail = command_output_detail(&output.stdout, &output.stderr);
    match classify_online_failure(&detail) {
        OnlineFailure::HostKey => Err(Error::OnlineHostKey {
            remote: remote.clone(),
            detail,
        }),
        OnlineFailure::Authorization => Err(Error::OnlineAuthorization {
            remote: remote.clone(),
            detail,
        }),
        OnlineFailure::Other => Err(Error::OnlineVerify {
            remote: remote.clone(),
            detail,
        }),
    }
}

fn online_verification_cwd(root: &Path, common_dir: &Path) -> Result<PathBuf, Error> {
    let integration = root.join(".loom").join("integration");
    if !integration.is_dir() {
        return Ok(root.to_path_buf());
    }
    let integration_common_dir = git_common_dir(&integration)?;
    if integration_common_dir == common_dir {
        return Ok(integration);
    }
    Err(Error::OnlineWorktreeCommonDir {
        path: path_string(&integration),
        expected: path_string(common_dir),
        actual: path_string(&integration_common_dir),
    })
}

fn require_runtime_git_config(cwd: &Path, key: &str, expected: &str) -> Result<(), Error> {
    let output = ProcessCommand::new("git")
        .arg("config")
        .arg("--get")
        .arg(key)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .map_err(Error::GitIo)?;
    if !output.status.success() {
        return Err(Error::GitConfigQuery {
            key: key.to_owned(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if value == expected {
        return Ok(());
    }
    Err(Error::RuntimeConfigMismatch {
        key: key.to_owned(),
        value,
        expected: expected.to_owned(),
        cwd: path_string(cwd),
    })
}

fn online_git_ls_remote(cwd: &Path, remote: &RemoteName) -> Result<std::process::Output, Error> {
    let mut command = ProcessCommand::new("git");
    command
        .arg("ls-remote")
        .arg(remote.as_str())
        .arg("HEAD")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env_clear()
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_SSH_VARIANT", "ssh");
    copy_env_if_present(&mut command, "PATH");
    copy_env_if_present(&mut command, "HOME");
    copy_env_if_present(&mut command, "WRIX_DEPLOY_KEY");
    command.output().map_err(Error::GitIo)
}

fn copy_env_if_present(command: &mut ProcessCommand, name: &'static str) {
    if let Some(value) = env::var_os(name) {
        command.env(name, value);
    }
}

fn command_output_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::from("git ls-remote exited non-zero without output"),
        (false, true) => stdout,
        (true, false) => stderr,
        (false, false) => format!("{stdout}\n{stderr}"),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OnlineFailure {
    HostKey,
    Authorization,
    Other,
}

fn classify_online_failure(detail: &str) -> OnlineFailure {
    let detail = detail.to_ascii_lowercase();
    if detail.contains("host key verification failed")
        || detail.contains("no matching host key")
        || detail.contains("no hostkey alg")
        || detail.contains("no ed25519 host key is known")
        || detail.contains("no ecdsa host key is known")
        || detail.contains("no rsa host key is known")
        || detail.contains("remote host identification has changed")
    {
        return OnlineFailure::HostKey;
    }
    if detail.contains("permission denied (publickey)")
        || detail.contains("repository not found")
        || detail.contains("authentication failed")
        || detail.contains("access denied")
    {
        return OnlineFailure::Authorization;
    }
    OnlineFailure::Other
}

fn ensure_context_stable_ssh_command(root: &Path, value: &str) -> Result<(), Error> {
    let root = path_string(root);
    let forbidden = [
        ("/nix/store", "host-only Nix store path"),
        ("/etc/wrix/keys", "container private-key path"),
        ("/workspace", "container workspace path"),
        (".ssh/deploy_keys", "private-key lookup path"),
        ("WRIX_DEPLOY_KEY", "environment private-key path"),
    ];
    if !root.is_empty() && root != "/" && value.contains(&root) {
        return Err(Error::TransportConfigUnstable {
            value: value.to_owned(),
            reason: String::from("workspace path"),
        });
    }
    for (needle, reason) in forbidden {
        if value.contains(needle) {
            return Err(Error::TransportConfigUnstable {
                value: value.to_owned(),
                reason: String::from(reason),
            });
        }
    }
    Ok(())
}

fn require_mode(path: impl AsRef<Path>, expected: u32) -> Result<(), Error> {
    let path = path.as_ref();
    let metadata = fs::metadata(path).map_err(|source| Error::StateIo {
        path: path_string(path),
        source,
    })?;
    let actual = metadata.permissions().mode() & 0o777;
    if actual == expected {
        return Ok(());
    }
    Err(Error::PermissionMode {
        path: path_string(path),
        expected: format!("{expected:04o}"),
        actual: format!("{actual:04o}"),
    })
}

fn require_private_key_mode(kind: &'static str, path: &Path) -> Result<(), Error> {
    let metadata = fs::metadata(path).map_err(|source| Error::StateIo {
        path: path_string(path),
        source,
    })?;
    let actual = metadata.permissions().mode() & 0o777;
    let group_or_other_mode = actual % 0o100;
    let owner_can_read = actual >= 0o400;
    if metadata.is_file() && owner_can_read && group_or_other_mode == 0 {
        return Ok(());
    }
    Err(Error::PrivateKeyMode {
        kind,
        path: path_string(path),
        actual: format!("{actual:04o}"),
    })
}

fn resolved_deploy_key(key_name: &KeyName) -> Result<PathBuf, Error> {
    if let Ok(value) = env::var("WRIX_DEPLOY_KEY")
        && !value.trim().is_empty()
    {
        let path = PathBuf::from(value);
        if path.is_file() {
            return path.canonicalize().map_err(|source| Error::StateIo {
                path: path_string(&path),
                source,
            });
        }
        return Err(Error::DeployKeyEnvMissing {
            path: path_string(&path),
        });
    }
    let home = env::var("HOME").map_err(|source| Error::HomeMissing { source })?;
    if home.trim().is_empty() {
        return Err(Error::HomeEmpty);
    }
    let path = Path::new(&home)
        .join(".ssh")
        .join("deploy_keys")
        .join(key_name.to_string());
    if path.is_file() {
        return path.canonicalize().map_err(|source| Error::StateIo {
            path: path_string(&path),
            source,
        });
    }
    Err(Error::DeployKeyMissing {
        path: path_string(&path),
    })
}

fn probe_helper_config(root: &Path, helper_path: &Path) -> Result<String, Error> {
    let output = ProcessCommand::new(helper_path)
        .arg("-G")
        .arg("github.com")
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(|source| Error::HelperIo {
            path: path_string(helper_path),
            source,
        })?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    Err(Error::HelperVerify {
        detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

fn require_ssh_config_value(output: &str, key: &str, expected: &str) -> Result<(), Error> {
    require_ssh_config_value_any(output, key, &[expected])
}

fn require_ssh_config_value_any(output: &str, key: &str, expected: &[&str]) -> Result<(), Error> {
    for value in ssh_config_values(output, key) {
        if expected.contains(&value) {
            return Ok(());
        }
    }
    Err(Error::HelperVerify {
        detail: format!("ssh -G output is missing {key} = {}", expected.join(" or ")),
    })
}

fn ssh_config_values<'a>(output: &'a str, key: &str) -> Vec<&'a str> {
    output
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(' ')?;
            if name == key {
                Some(value.trim())
            } else {
                None
            }
        })
        .collect()
}

fn transport_helper_script(key_name: &KeyName) -> String {
    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

key_name={key_name}

fail() {{
  local message="$1"
  printf 'wrix git ssh: %s\n' "$message" >&2
  exit 255
}}

absolute_path() {{
  local input="$1"
  local dir base
  dir="$(dirname "$input")"
  base="$(basename "$input")"
  dir="$(cd "$dir" && pwd -P)"
  printf '%s/%s\n' "$dir" "$base"
}}

resolve_key() {{
  local candidate
  if [[ -n "${{WRIX_DEPLOY_KEY:-}}" ]]; then
    if [[ -f "$WRIX_DEPLOY_KEY" ]]; then
      absolute_path "$WRIX_DEPLOY_KEY"
      return 0
    fi
    fail "WRIX_DEPLOY_KEY does not point at a file: $WRIX_DEPLOY_KEY"
  fi
  if [[ -z "${{HOME:-}}" ]]; then
    fail "HOME is not set and WRIX_DEPLOY_KEY is unset"
  fi
  candidate="$HOME/.ssh/deploy_keys/$key_name"
  if [[ -f "$candidate" ]]; then
    absolute_path "$candidate"
    return 0
  fi
  fail "no deploy key resolved; set WRIX_DEPLOY_KEY or create $candidate"
}}

script_dir="$(cd "$(dirname "${{BASH_SOURCE[0]}}")" && pwd -P)"
known_hosts="$script_dir/github_known_hosts"
if [[ ! -f "$known_hosts" ]]; then
  fail "pinned GitHub known-hosts file is missing: $known_hosts"
fi
key_path="$(resolve_key)"
exec ssh -F /dev/null -o BatchMode=yes -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o "UserKnownHostsFile=$known_hosts" -o GlobalKnownHostsFile=/dev/null -o IdentityAgent=none -o IdentityFile=none -i "$key_path" "$@"
"#,
        key_name = shell_single_quote(&key_name.to_string())
    )
}

fn shell_single_quote(value: &str) -> String {
    let mut quoted = String::from("'");
    for character in value.chars() {
        if character == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
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
    /// cannot resolve Git common directory: {detail}
    GitCommonDir { detail: String },
    /// cannot write Wrix Git state at {path}: {source}
    StateIo { path: String, source: io::Error },
    /// cannot set {path} permissions to {mode}: {source}
    Permission {
        path: String,
        mode: String,
        source: io::Error,
    },
    /// Wrix Git state {path} has mode {actual}, expected {expected}
    PermissionMode {
        path: String,
        expected: String,
        actual: String,
    },
    /// {kind} {path} has mode {actual}; expected owner-readable mode with no group or other permissions
    PrivateKeyMode {
        kind: &'static str,
        path: String,
        actual: String,
    },
    /// failed to write Git config {key}: {detail}
    GitConfig { key: String, detail: String },
    /// failed to read Git config {key}: {detail}
    GitConfigQuery { key: String, detail: String },
    /// {0}
    Signing(#[from] crate::sign::Error),
    /// cannot execute wrix-git-sign from PATH: {source}
    SigningProgramIo { source: io::Error },
    /// wrix-git-sign PATH probe failed: {detail}
    SigningProgramProbe { detail: String },
    /// signing Git config {key} is not context-stable ({reason}): {value}
    SigningConfigUnstable {
        key: String,
        value: String,
        reason: String,
    },
    /// signing Git config {key} is {value}, expected {expected}
    SigningConfigMismatch {
        key: String,
        value: String,
        expected: String,
    },
    /// {name} is not valid Unicode: {source}
    IdentityEnvInvalid {
        name: &'static str,
        source: env::VarError,
    },
    /// Git signing verification command failed ({command}): {detail}
    GitCommand { command: String, detail: String },
    /// signed test commit failed: {detail}
    SigningCommit { detail: String },
    /// signed test commit did not verify: {detail}
    SigningVerify { detail: String },
    /// core.sshCommand is not context-stable ({reason}): {value}
    TransportConfigUnstable { value: String, reason: String },
    /// core.sshCommand does not match the Wrix common-dir trampoline: {value}
    TransportConfigMismatch { value: String },
    /// runtime Git config {key} in {cwd} is {value}, expected {expected}
    RuntimeConfigMismatch {
        key: String,
        value: String,
        expected: String,
        cwd: String,
    },
    /// online verification worktree {path} uses common dir {actual}, expected {expected}
    OnlineWorktreeCommonDir {
        path: String,
        expected: String,
        actual: String,
    },
    /// online verification failed host-key verification for remote '{remote}': {detail}
    OnlineHostKey { remote: RemoteName, detail: String },
    /// online verification reached GitHub but authentication or repository authorization failed for remote '{remote}': {detail}
    OnlineAuthorization { remote: RemoteName, detail: String },
    /// online verification failed for remote '{remote}': {detail}
    OnlineVerify { remote: RemoteName, detail: String },
    /// hook setup is enabled but WRIX_PREK_HOOKS is not set; use the Nix-packaged wrix or pass --no-hooks
    PrekHooksEnvMissing,
    /// WRIX_PREK_HOOKS is not valid Unicode: {source}
    PrekHooksEnvInvalid { source: env::VarError },
    /// WRIX_PREK_HOOKS is empty; use the Nix-packaged wrix or pass --no-hooks
    PrekHooksEnvEmpty,
    /// cannot read Wrix prek hook bundle at {path}: {source}
    PrekHooksIo { path: String, source: io::Error },
    /// Wrix prek hook bundle is not a directory: {path}
    PrekHooksNotDirectory { path: String },
    /// Wrix prek hook is missing or not executable: {path}
    PrekHookNotExecutable { path: String },
    /// core.hooksPath is {value}, expected Wrix prek hook bundle {expected}
    PrekHooksConfigMismatch { value: String, expected: String },
    /// WRIX_DEPLOY_KEY does not point at a file: {path}
    DeployKeyEnvMissing { path: String },
    /// HOME is not set; cannot resolve fallback deploy key: {source}
    HomeMissing { source: env::VarError },
    /// HOME is empty; cannot resolve fallback deploy key
    HomeEmpty,
    /// fallback deploy key does not exist: {path}
    DeployKeyMissing { path: String },
    /// cannot execute Wrix Git transport helper {path}: {source}
    HelperIo { path: String, source: io::Error },
    /// Wrix Git transport helper verification failed: {detail}
    HelperVerify { detail: String },
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
        DeployPolicy, FilePolicy, ForcePolicy, HookPolicy, KeyName, OnlineFailure, RemoteName,
        SigningPolicy, VerificationPolicy, classify_online_failure, parse_file_policy, parse_flags,
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

    #[test]
    fn online_failure_classification_distinguishes_host_key_and_auth() {
        assert_eq!(
            classify_online_failure("Host key verification failed."),
            OnlineFailure::HostKey,
        );
        assert_eq!(
            classify_online_failure(
                "ERROR: Repository not found.\nfatal: Could not read from remote repository."
            ),
            OnlineFailure::Authorization,
        );
        assert_eq!(
            classify_online_failure("ssh: Could not resolve hostname github.com"),
            OnlineFailure::Other,
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
