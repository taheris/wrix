use crate::process::Owned;
use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::OnceLock,
    time::Duration,
};

#[derive(Clone, Copy, Debug)]
pub enum Agent {
    Direct,
    Claude,
    Pi,
}
impl Agent {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Claude => "claude",
            Self::Pi => "pi",
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Mode {
    Run,
    Spawn,
}
impl Mode {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Spawn => "spawn",
        }
    }
}

pub struct Context {
    pub launcher: PathBuf,
    pub profiles: [(Agent, PathBuf); 3],
    pub runtime: PathBuf,
    pub runtime_name: &'static str,
}

pub fn checked(output: Output) -> Vec<u8> {
    assert!(output.status.success(), "command failed: {output:?}");
    output.stdout
}

pub fn command(command: &mut Command, limit: Duration) -> Output {
    Owned::spawn(command).finish_with_limit(b"", limit)
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/security")
        .join(name)
}

pub fn environment() -> BTreeMap<OsString, OsString> {
    env::vars_os()
        .filter(|(key, _)| {
            let key = key.as_encoded_bytes();
            !key.starts_with(b"WRIX_")
                && !key.starts_with(b"BEADS_DOLT_SERVER_")
                && key != b"TMUX"
                && key != b"TMUX_PANE"
        })
        .collect()
}

fn output_path(command: &mut Command, limit: Duration) -> PathBuf {
    let text = String::from_utf8(checked(self::command(command, limit))).unwrap();
    let path = PathBuf::from(text.trim());
    assert!(
        path.is_absolute(),
        "expected one absolute output path: {text:?}"
    );
    assert!(path.exists(), "missing output: {}", path.display());
    path
}

pub fn packaged() -> &'static Context {
    static CONTEXT: OnceLock<Context> = OnceLock::new();
    CONTEXT.get_or_init(|| {
        let system = String::from_utf8(checked(command(
            Command::new("nix").args([
                "eval",
                "--raw",
                "--impure",
                "--expr",
                "builtins.currentSystem",
            ]),
            Duration::from_secs(120),
        )))
        .unwrap();
        let build = |attribute: &str| {
            output_path(
                Command::new("nix").args([
                    "build",
                    "--no-link",
                    "--print-out-paths",
                    "--no-warn-dirty",
                    attribute,
                ]),
                Duration::from_mins(30),
            )
        };
        let runtime_name = if cfg!(target_os = "macos") {
            "container"
        } else {
            "podman"
        };
        let runtime = env::split_paths(&env::var_os("PATH").unwrap())
            .map(|directory| directory.join(runtime_name))
            .find(|path| {
                path.is_file() && fs::metadata(path).unwrap().permissions().mode() & 0o111 != 0
            })
            .expect("actual container runtime must be available");
        Context {
            launcher: build(".#wrix").join("bin/wrix"),
            profiles: [
                (
                    Agent::Direct,
                    build(&format!(
                        ".#legacyPackages.{}.testFixtures.execution.image.profileConfig",
                        system.trim()
                    )),
                ),
                (
                    Agent::Claude,
                    build(".#image-base-claude.image.profileConfig"),
                ),
                (Agent::Pi, build(".#image-base-pi.image.profileConfig")),
            ],
            runtime: fs::canonicalize(runtime).unwrap(),
            runtime_name,
        }
    })
}
