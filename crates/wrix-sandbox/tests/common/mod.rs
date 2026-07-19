use std::{
    env,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use serde_json::{Value, json};
use wrix_sandbox::command::{self, Command};

pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

pub struct ChildSpec {
    pub command: Command,
    pub profile_config: Option<PathBuf>,
    pub args: Vec<String>,
    pub env: Vec<(String, OsString)>,
}

pub struct ChildRun {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

pub struct ProfileFixture {
    pub agent_kind: String,
    pub deploy_key: Option<String>,
    pub mounts: Vec<Value>,
    pub source_kind: String,
}

impl Default for ProfileFixture {
    fn default() -> Self {
        Self {
            agent_kind: String::from("direct"),
            deploy_key: None,
            mounts: Vec::new(),
            source_kind: String::from(expected_source_kind()),
        }
    }
}

pub fn run_child(
    child_name: &str,
    root: &Path,
    label: &str,
    spec: ChildSpec,
) -> TestResult<ChildRun> {
    let stdout = root.join(format!("{label}.out"));
    let stderr = root.join(format!("{label}.err"));
    let status = root.join(format!("{label}.status"));
    fs::create_dir_all(root.join("home"))?;
    fs::create_dir_all(root.join("cache"))?;
    fs::create_dir_all(root.join("runtime"))?;

    let mut command = ProcessCommand::new(env::current_exe()?);
    command
        .arg(child_name)
        .arg("--exact")
        .arg("--ignored")
        .env("WRIX_SANDBOX_CHILD_COMMAND", command_name(spec.command))
        .env("WRIX_SANDBOX_STDOUT", &stdout)
        .env("WRIX_SANDBOX_STDERR", &stderr)
        .env("WRIX_SANDBOX_STATUS", &status)
        .env("WRIX_SANDBOX_ARG_COUNT", spec.args.len().to_string())
        .env("WRIX_DRY_RUN", "1")
        .env("HOME", root.join("home"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"))
        .env("GIT_AUTHOR_NAME", "Wrix Test")
        .env("GIT_AUTHOR_EMAIL", "wrix@example.test")
        .env("GIT_COMMITTER_NAME", "Wrix Test")
        .env("GIT_COMMITTER_EMAIL", "wrix@example.test")
        .env_remove("WRIX_AGENT")
        .env_remove("WRIX_NETWORK")
        .env_remove("WRIX_MICROVM")
        .env_remove("WRIX_PODMAN_SOCKET")
        .env_remove("WRIX_UNSAFE_PODMAN_SOCKET")
        .env_remove("WRIX_DEPLOY_KEY")
        .env_remove("WRIX_SIGNING_KEY")
        .env_remove("WRIX_GIT_SIGN")
        .env_remove("WRIX_MCP")
        .env_remove("WRIX_MCP_TMUX_AUDIT")
        .env_remove("WRIX_MCP_TMUX_AUDIT_FULL")
        .env_remove("WRIX_PI_AUTH_FILE")
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
        .env_remove("TMUX");
    if let Some(profile_config) = spec.profile_config {
        command.env("WRIX_SANDBOX_PROFILE_CONFIG", profile_config);
    }
    for (index, arg) in spec.args.iter().enumerate() {
        command.env(format!("WRIX_SANDBOX_ARG_{index}"), arg);
    }
    for (key, value) in spec.env {
        command.env(key, value);
    }

    let output = command.output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }

    Ok(ChildRun {
        stdout: fs::read_to_string(stdout)?,
        stderr: fs::read_to_string(stderr)?,
        success: fs::read_to_string(status)?.trim() == "0",
    })
}

pub fn run_command_child() -> TestResult {
    let command_name = env::var("WRIX_SANDBOX_CHILD_COMMAND")?;
    let command = Command::parse(&command_name)
        .ok_or_else(|| io::Error::other(format!("unknown command {command_name}")))?;
    let profile_config = env::var_os("WRIX_SANDBOX_PROFILE_CONFIG").map(PathBuf::from);
    let arg_count = env::var("WRIX_SANDBOX_ARG_COUNT")?.parse::<usize>()?;
    let mut args = Vec::with_capacity(arg_count);
    for index in 0..arg_count {
        args.push(env::var(format!("WRIX_SANDBOX_ARG_{index}"))?);
    }

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = command::run(command, profile_config, &args, &mut stdout, &mut stderr)?;
    fs::write(PathBuf::from(env::var("WRIX_SANDBOX_STDOUT")?), stdout)?;
    fs::write(PathBuf::from(env::var("WRIX_SANDBOX_STDERR")?), stderr)?;
    let status = if code == std::process::ExitCode::SUCCESS {
        "0\n"
    } else {
        "1\n"
    };
    fs::write(PathBuf::from(env::var("WRIX_SANDBOX_STATUS")?), status)?;
    Ok(())
}

pub fn write_profile_config(path: &Path, fixture: &ProfileFixture) -> TestResult {
    let value = json!({
        "schema": 1,
        "system": "test",
        "profile": {
            "name": "base",
            "env": {},
            "mounts": fixture.mounts,
            "writable_dirs": [],
            "network_allowlist": ["example.org"]
        },
        "image": {
            "ref": "localhost/wrix-test:latest",
            "source": "/nix/store/fake-image",
            "source_kind": fixture.source_kind,
            "digest": format!("sha256:{}", "a".repeat(64))
        },
        "agent": { "kind": fixture.agent_kind },
        "resources": { "cpus": null, "memory_mb": 4096, "pids_limit": 4096 },
        "security": { "deploy_key": fixture.deploy_key },
        "network": { "default_mode": "open", "ipv6": "disabled" },
        "services": { "beads": { "enable": "auto" }, "nix_cache": { "enable": false } },
        "features": { "mcp_runtime": false }
    });
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(&value)?))?;
    Ok(())
}

pub const fn expected_source_kind() -> &'static str {
    if cfg!(target_os = "macos") {
        "docker-archive"
    } else {
        "nix-descriptor"
    }
}

const fn command_name(command: Command) -> &'static str {
    match command {
        Command::Run => "run",
        Command::Spawn => "spawn",
    }
}
