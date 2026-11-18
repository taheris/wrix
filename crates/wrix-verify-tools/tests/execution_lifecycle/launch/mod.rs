use super::context::{self, Agent, Context, Mode};
use crate::process::Owned;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

pub struct Launch {
    pub process: Option<Owned>,
    pub control: PathBuf,
    pub name: String,
    pub member: String,
    pub workspace: PathBuf,
    pub mode: Mode,
    pub agent: Agent,
    pub record: PathBuf,
    pub initial: Vec<u8>,
}

pub struct Harness {
    root: tempfile::TempDir,
    context: &'static Context,
    env: BTreeMap<OsString, OsString>,
    profiles: BTreeMap<&'static str, PathBuf>,
    pub launches: Vec<Launch>,
}

pub fn json_file(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

impl Harness {
    fn new() -> Self {
        let context = context::packaged();
        let root = tempfile::Builder::new()
            .prefix("wrix-execution-live-")
            .tempdir()
            .unwrap();
        let mut env = context::environment();
        env.insert(
            "XDG_CACHE_HOME".into(),
            root.path().join("cache").into_os_string(),
        );
        fs::create_dir(root.path().join("bin")).unwrap();
        symlink(
            context::fixture("execution-runtime.sh"),
            root.path().join("bin").join(context.runtime_name),
        )
        .unwrap();
        let auth = root.path().join("auth.json");
        fs::write(&auth, "{}\n").unwrap();
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o600)).unwrap();
        let mut profiles = BTreeMap::new();
        for (agent, source) in &context.profiles {
            let mut profile = json_file(source);
            profile["services"]["nix_cache"]["enable"] = json!(false);
            let path = root.path().join(format!("profile-{}.json", agent.name()));
            fs::write(&path, serde_json::to_vec(&profile).unwrap()).unwrap();
            profiles.insert(agent.name(), path);
        }
        Self {
            root,
            context,
            env,
            profiles,
            launches: Vec::new(),
        }
    }

    pub fn workspace(&self, name: &str) -> PathBuf {
        let workspace = self.root.path().join(name);
        fs::create_dir(&workspace).unwrap();
        fs::copy(
            context::fixture("execution-probe.sh"),
            workspace.join("probe.sh"),
        )
        .unwrap();
        workspace
    }

    pub fn mismatch_profile(&self) -> PathBuf {
        let mut profile = json_file(&self.profiles["direct"]);
        profile["agent"]["kind"] = json!("claude");
        let path = self.root.path().join("mismatched-profile.json");
        fs::write(&path, serde_json::to_vec(&profile).unwrap()).unwrap();
        path
    }

    pub fn runtime(&self, args: &[&OsStr]) -> Output {
        context::command(
            Command::new(&self.context.runtime)
                .args(args)
                .env_clear()
                .envs(&self.env),
            Duration::from_secs(30),
        )
    }

    pub fn start(
        &mut self,
        mode: Mode,
        workspace: &Path,
        agent: Agent,
        status: i32,
        profile: Option<&Path>,
        release: bool,
    ) -> usize {
        let index = self.launches.len();
        let member = format!("launch-{index}");
        let control = self.root.path().join(&member);
        fs::create_dir(&control).unwrap();
        let nonce = self.root.path().file_name().unwrap().to_str().unwrap();
        let name = format!("wrix-execution-{}-{nonce}-{member}", std::process::id());
        let args = match agent {
            Agent::Direct => vec![
                "bash".to_owned(),
                "/workspace/probe.sh".to_owned(),
                "/workspace".to_owned(),
                member.clone(),
                status.to_string(),
            ],
            Agent::Claude => vec!["--version".to_owned()],
            Agent::Pi => vec!["--help".to_owned()],
        };
        let mut invocation = vec![
            self.context.launcher.as_os_str().to_owned(),
            "--profile-config".into(),
            profile
                .unwrap_or(&self.profiles[agent.name()])
                .as_os_str()
                .to_owned(),
        ];
        match mode {
            Mode::Spawn => {
                let config = control.join("spawn.json");
                fs::write(&config, serde_json::to_vec(&json!({"workspace": workspace, "env": [], "mounts": [], "git": {"deploy": false, "sign": false}, "bead_id": member, "agent_args": args})).unwrap()).unwrap();
                invocation.extend([
                    "spawn".into(),
                    "--spawn-config".into(),
                    config.into_os_string(),
                ]);
            }
            Mode::Run => {
                invocation.extend([
                    "run".into(),
                    "--no-git-deploy".into(),
                    "--no-git-sign".into(),
                    workspace.as_os_str().to_owned(),
                    "--".into(),
                ]);
                invocation.extend(args.into_iter().map(OsString::from));
            }
        }
        let mut command = if matches!(mode, Mode::Run) {
            let rendered = shell_join(&invocation);
            let mut command = Command::new("script");
            if self.context.runtime_name == "podman" {
                command.args(["-qefc", &rendered, "/dev/null"]);
            } else {
                command.args(["-q", "/dev/null", "bash", "-c", &rendered]);
            }
            command
        } else {
            let mut command = Command::new(&invocation[0]);
            command.args(&invocation[1..]);
            command
        };
        let mut path = vec![self.root.path().join("bin")];
        path.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        command
            .env_clear()
            .envs(&self.env)
            .env("WRIX_PI_AUTH_FILE", self.root.path().join("auth.json"))
            .env("WRIX_FOCUS_TARGET", format!("focus:{member}"))
            .env("WRIX_TEST_REAL_RUNTIME", &self.context.runtime)
            .env("WRIX_TEST_CONTROL", &control)
            .env("WRIX_TEST_CONTAINER_NAME", &name)
            .env("PATH", std::env::join_paths(path).unwrap());
        if self.context.runtime_name == "podman" {
            command.env(
                "WRIX_CONTAINER_RUNTIME",
                self.root.path().join("bin/podman"),
            );
        }
        let mut child = Owned::spawn(&mut command);
        child.close_stdin();
        assert_ne!(child.id(), std::process::id());
        self.launches.push(Launch {
            process: Some(child),
            control,
            name,
            member,
            workspace: workspace.to_owned(),
            mode,
            agent,
            record: PathBuf::new(),
            initial: Vec::new(),
        });
        let launch = &mut self.launches[index];
        launch
            .process
            .as_ref()
            .unwrap()
            .ready(&launch.control.join("ready"));
        let record = fs::read_dir(workspace.join(".wrix/log"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
                    && json_file(path)["focus_target"] == format!("focus:{}", launch.member)
            })
            .expect("launch did not establish its execution record");
        launch.initial = fs::read(&record).unwrap();
        launch.record = record;
        assert_initial(launch);
        if release {
            fs::write(launch.control.join("release"), "").unwrap();
        }
        index
    }

    pub fn probe_ready(&self, index: usize) {
        let launch = &self.launches[index];
        launch
            .process
            .as_ref()
            .unwrap()
            .ready(&launch.workspace.join(format!("ready-{}", launch.member)));
    }

    pub fn finish(&mut self, index: usize, expected: i32) -> (Value, String) {
        let launch = &mut self.launches[index];
        let output = launch.process.take().unwrap().finish(b"");
        let log = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if self.context.runtime_name == "podman" || matches!(launch.mode, Mode::Spawn) {
            assert_eq!(output.status.code(), Some(expected), "{log}");
        }
        let value = json_file(&launch.record);
        assert_completed(
            &serde_json::from_slice(&launch.initial).unwrap(),
            &value,
            expected,
        );
        (value, log)
    }

    pub fn present(&self, index: usize) -> bool {
        let name = self.launches[index].name.as_str();
        if self.context.runtime_name == "podman" {
            let output = self.runtime(&["container".as_ref(), "exists".as_ref(), name.as_ref()]);
            assert!(matches!(output.status.code(), Some(0 | 1)), "{output:?}");
            output.status.success()
        } else {
            let entries: Vec<Value> = serde_json::from_slice(&context::checked(self.runtime(&[
                "list".as_ref(),
                "--all".as_ref(),
                "--format".as_ref(),
                "json".as_ref(),
            ])))
            .unwrap();
            entries
                .iter()
                .any(|entry| entry["configuration"]["id"] == name)
        }
    }

    pub fn remove(&self, index: usize) -> Result<(), String> {
        let name = self.launches[index].name.as_str();
        if self.context.runtime_name == "podman" {
            let present = self.runtime(&["container".as_ref(), "exists".as_ref(), name.as_ref()]);
            match present.status.code() {
                Some(1) => return Ok(()),
                Some(0) => {}
                _ => return Err(format!("container presence failed: {present:?}")),
            }
            let removed = self.runtime(&[
                "rm".as_ref(),
                "--force".as_ref(),
                "--time".as_ref(),
                "0".as_ref(),
                name.as_ref(),
            ]);
            if !removed.status.success() {
                return Err(format!("owned container removal failed: {removed:?}"));
            }
        } else {
            let listing = self.runtime(&[
                "list".as_ref(),
                "--all".as_ref(),
                "--format".as_ref(),
                "json".as_ref(),
            ]);
            if !listing.status.success() {
                return Err(format!("container list failed: {listing:?}"));
            }
            let entries: Vec<Value> =
                serde_json::from_slice(&listing.stdout).map_err(|error| error.to_string())?;
            if entries
                .iter()
                .any(|entry| entry["configuration"]["id"] == name)
            {
                let removed = self.runtime(&["delete".as_ref(), "--force".as_ref(), name.as_ref()]);
                if !removed.status.success() {
                    return Err(format!("owned container removal failed: {removed:?}"));
                }
            }
        }
        Ok(())
    }

    fn cleanup(&mut self) -> Vec<String> {
        let mut failures = Vec::new();
        for index in 0..self.launches.len() {
            if let Some(mut process) = self.launches[index].process.take() {
                match catch_unwind(AssertUnwindSafe(|| process.stop())) {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => failures.push(error.to_string()),
                    Err(_) => {
                        failures.push(format!("owned process cleanup panicked for launch {index}"));
                    }
                }
            }
            match catch_unwind(AssertUnwindSafe(|| self.remove(index))) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failures.push(error),
                Err(_) => failures.push(format!(
                    "owned container cleanup panicked for launch {index}"
                )),
            }
        }
        failures
    }
}

pub fn with_live(test: impl FnOnce(&mut Harness)) {
    let mut harness = Harness::new();
    let result = catch_unwind(AssertUnwindSafe(|| test(&mut harness)));
    let cleanup = harness.cleanup();
    if let Err(error) = result {
        if !cleanup.is_empty() {
            eprintln!("independent container cleanup failed: {cleanup:?}");
        }
        resume_unwind(error);
    }
    assert_eq!(
        cleanup,
        Vec::<String>::new(),
        "independent container cleanup failed"
    );
}

pub fn assert_initial(launch: &Launch) {
    let value: Value = serde_json::from_slice(&launch.initial).unwrap();
    assert_eq!(value["state"], "incomplete");
    assert_eq!(
        launch.record.file_name().unwrap().to_str().unwrap(),
        format!("{}.json", value["execution_id"].as_str().unwrap())
    );
    for key in [
        "timestamp_end",
        "duration_seconds",
        "exit_code",
        "signal",
        "agent_session_id",
    ] {
        assert_eq!(value.get(key), Some(&Value::Null));
    }
    assert!(value.get("bead_id").is_some());
    assert_eq!(value["mode"], launch.mode.name());
    assert_eq!(value["agent_kind"], launch.agent.name());
    assert_eq!(
        value["bead_id"],
        if matches!(launch.mode, Mode::Spawn) {
            json!(launch.member)
        } else {
            Value::Null
        }
    );
}

pub fn assert_completed(initial: &Value, value: &Value, expected: i32) {
    assert_eq!(value["state"], "completed");
    assert_eq!(value["exit_code"], expected);
    assert_eq!(value.get("signal"), Some(&Value::Null));
    assert!(value["timestamp_end"].is_string());
    assert!(value["duration_seconds"].as_f64().unwrap() >= 0.0);
    for (key, before) in initial.as_object().unwrap() {
        if ![
            "state",
            "timestamp_end",
            "duration_seconds",
            "exit_code",
            "signal",
        ]
        .contains(&key.as_str())
        {
            assert_eq!(
                value.get(key),
                Some(before),
                "completion changed or removed {key}"
            );
        }
    }
}

pub fn shell_join(arguments: &[OsString]) -> String {
    arguments
        .iter()
        .map(|argument| format!("'{}'", argument.to_str().unwrap().replace('\'', "'\"'\"'")))
        .collect::<Vec<_>>()
        .join(" ")
}
