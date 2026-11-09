//! Actual host shell integration with private native service and Nix fixtures.
use super::*;
use common::RunResult;
use std::{collections::BTreeMap, ffi::OsString, path::Path};

struct Host {
    fixture: Fixture,
    source: PathBuf,
    policy: PathBuf,
    store: PathBuf,
    bash: PathBuf,
}

impl Host {
    fn new() -> TestResult<Self> {
        let fixture = Fixture::new()?;
        let root = fixture.root.path();
        let source = root.join("host-nix-config.sh");
        fs::write(
            &source,
            include_str!("../../../../lib/services/host-nix-config.sh"),
        )?;
        let policy = std::env::var_os("WRIX_HOST_CACHE_POLICY_BIN").map_or_else(
            || PathBuf::from(env!("CARGO_BIN_EXE_wrix")).with_file_name("wrix-host-cache-policy"),
            PathBuf::from,
        );
        assert!(policy.is_file(), "native host cache policy must be built");
        let cli = root.join("host-wrix");
        fs::write(
            &cli,
            format!(
                "#!/usr/bin/env bash\nset -euo pipefail\nexec bash {} {} \"$@\"\n",
                common::shell_single_quote(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/fixtures/host-command.sh"
                )),
                common::shell_single_quote(env!("CARGO_BIN_EXE_wrix"))
            ),
        )?;
        set_mode(&cli, 0o755)?;
        let store_root = root.join("isolated-store");
        let initializer = std::env::var_os("WRIX_TEST_HOST_HOOK_STORE_INIT")
            .ok_or("native host-hook store initializer must be prepared")?;
        let initialized = run_command(Command::new(initializer).arg(&store_root))?;
        assert!(initialized.status.success(), "{}", initialized.stderr);
        assert_eq!(initialized.stdout.lines().count(), 1);
        let bash = PathBuf::from(initialized.stdout.trim());
        assert!(bash.is_absolute() && bash.is_file());
        let store = root.join("hook-nix-store");
        fs::write(
            &store,
            format!(
                "#!/usr/bin/env bash\nset -euo pipefail\nexec {} --store {} \"$@\"\n",
                common::shell_single_quote(
                    common::command_path("nix-store")?
                        .to_str()
                        .ok_or("non-Unicode Nix store binary")?
                ),
                common::shell_single_quote(store_root.to_str().ok_or("non-Unicode store root")?)
            ),
        )?;
        set_mode(&store, 0o755)?;
        Ok(Self {
            fixture,
            source,
            policy,
            store,
            bash,
        })
    }

    fn start(&self) -> TestResult {
        let result = run_command(
            self.fixture
                .command("podman")?
                .args(["service", "start"])
                .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1"),
        )?;
        assert!(result.status.success(), "{}", result.stderr);
        Ok(())
    }

    fn shell(&self, body: &str) -> TestResult<Command> {
        let template = self.fixture.command("podman")?;
        let mut command = Command::new(common::command_path("bash")?);
        command.env_clear().current_dir(&self.fixture.workspace);
        for (name, value) in template.get_envs() {
            if let Some(value) = value {
                command.env(name, value);
            }
        }
        command
            .env(
                "WRIX_SERVICE_BIN",
                self.fixture.root.path().join("host-wrix"),
            )
            .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1")
            .env("WRIX_HOST_CACHE_POLICY_BIN", &self.policy)
            .env("WRIX_NIX_STORE_BIN", &self.store)
            .env("WRIX_BASH_BIN", &self.bash)
            .env("WRIX_CACHE_HOOK_BIN", "/bin/true")
            .env("WRIX_CACHE_PUBLISH_BIN", "/bin/true")
            .env("WRIX_NIX_CACHE_REMINDER", "0")
            .env("HOST_SOURCE", &self.source)
            .args(["-euc", body]);
        Ok(command)
    }

    fn activate(&self, config: &str) -> TestResult<RunResult> {
        run_command(
            self.shell("source \"$HOST_SOURCE\"; printf '%s\\n' \"$NIX_CONFIG\"")?
                .env("NIX_CONFIG", config),
        )
    }
}

fn settings(text: &str) -> BTreeMap<&str, &str> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.trim(), value.trim()))
        .collect()
}

#[test]
fn host_publication_hook_activation_preserves_host_trust_and_retains_both_generations() -> TestResult
{
    let host = Host::new()?;
    fs::write(
        host.fixture.workspace.join("wrix.toml"),
        "cache.pending_ttl = \"3d\"\n",
    )?;
    host.start()?;
    let baseline =
        "extra-substituters = https://cache.nixos.org/\nbuilders-use-substitutes = false";
    let first = host.activate(baseline)?;
    assert!(first.status.success(), "{}", first.stderr);
    let first_settings = settings(&first.stdout);
    assert_eq!(
        first_settings["extra-substituters"],
        "https://cache.nixos.org/"
    );
    assert_eq!(first_settings["builders-use-substitutes"], "false");
    assert!(!first_settings.contains_key("extra-trusted-public-keys"));
    let first_path = PathBuf::from(first_settings["post-build-hook"]);
    let first_store = first_path
        .parent()
        .and_then(Path::parent)
        .ok_or("missing wrapper store path")?;
    let physical = host
        .fixture
        .root
        .path()
        .join("isolated-store")
        .join(first_store.strip_prefix("/")?);
    let snapshot: Value = serde_json::from_slice(&fs::read(physical.join("cache-config.json"))?)?;
    assert_eq!(snapshot["pending_retention_secs"], 259_200);
    fs::write(
        host.fixture.workspace.join("wrix.toml"),
        "cache.pending_ttl = \"4d\"\n",
    )?;
    let second = host.activate(&first.stdout)?;
    assert!(second.status.success(), "{}", second.stderr);
    let second_settings = settings(&second.stdout);
    let second_path = PathBuf::from(second_settings["post-build-hook"]);
    assert_ne!(first_path, second_path);
    let second_store = second_path
        .parent()
        .and_then(Path::parent)
        .ok_or("missing wrapper store path")?;
    let endpoints = host.fixture.endpoints(true)?;
    let state = Path::new(
        endpoints["state_root"]
            .as_str()
            .ok_or("missing state root")?,
    );
    for output in [first_store, second_store] {
        assert_eq!(
            fs::read_link(
                state
                    .join("host-nix-roots/generations")
                    .join(output.file_name().ok_or("missing wrapper name")?)
                    .join("wrapper")
            )?,
            output
        );
    }
    let collected = Command::new(&host.store).arg("--gc").output()?;
    assert!(
        collected.status.success(),
        "{}",
        String::from_utf8_lossy(&collected.stderr)
    );
    assert!(
        Command::new(&host.store)
            .arg("--check-validity")
            .args([first_store, second_store])
            .status()?
            .success()
    );
    assert_eq!(
        settings(&second.stdout)["extra-substituters"],
        first_settings["extra-substituters"]
    );
    Ok(())
}

#[test]
fn host_publication_hook_rejects_conflicting_operator_hook_without_rewriting_it() -> TestResult {
    let host = Host::new()?;
    host.start()?;
    let output = host.activate("post-build-hook = /operator/hook")?;
    assert!(!output.status.success());
    assert!(
        output
            .stderr
            .contains("existing non-wrix post-build-hook conflicts"),
        "{}",
        output.stderr
    );
    Ok(())
}

#[test]
fn host_publication_hook_cache_opt_out_preserves_inherited_configuration() -> TestResult {
    let host = Host::new()?;
    fs::write(
        host.fixture.workspace.join("wrix.toml"),
        "cache.enable = false\n",
    )?;
    let inherited = "post-build-hook = /operator/hook\nmax-jobs = 1";
    let output = host.activate(inherited)?;
    assert!(output.status.success(), "{}", output.stderr);
    assert_eq!(output.stdout.trim_end(), inherited);
    Ok(())
}

#[test]
fn host_publication_hook_required_setting_refusal_is_not_silently_accepted() -> TestResult {
    let host = Host::new()?;
    host.start()?;
    let nix = host.fixture.root.path().join("ignoring-nix");
    fs::write(
        &nix,
        "#!/usr/bin/env bash\nset -euo pipefail\n[[ \"$*\" == 'config show' ]]\nprintf 'post-build-hook =\\n'\n",
    )?;
    set_mode(&nix, 0o755)?;
    let output = run_command(
        host.shell("source \"$HOST_SOURCE\"")?
            .env("NIX_CONFIG", "")
            .env("WRIX_NIX_BIN", &nix),
    )?;
    assert!(!output.status.success());
    assert!(
        output
            .stderr
            .contains("host Nix does not honor post-build-hook"),
        "{}",
        output.stderr
    );
    Ok(())
}

#[test]
fn host_publication_hook_repository_validation_policy_can_be_restored_by_environment() -> TestResult
{
    let host = Host::new()?;
    fs::write(
        host.fixture.workspace.join("wrix.toml"),
        "cache.require_trusted_nix = false\n",
    )?;
    host.start()?;
    let nix = host.fixture.root.path().join("ignoring-nix");
    fs::write(
        &nix,
        "#!/usr/bin/env bash\nset -euo pipefail\n[[ \"$*\" == 'config show' ]]\nprintf 'post-build-hook =\\n'\n",
    )?;
    set_mode(&nix, 0o755)?;
    for required in [false, true] {
        let mut command = host.shell("source \"$HOST_SOURCE\"")?;
        command.env("NIX_CONFIG", "").env("WRIX_NIX_BIN", &nix);
        if required {
            command.env("WRIX_NIX_CACHE_REQUIRE_TRUSTED", "1");
        }
        let output = run_command(&mut command)?;
        assert_eq!(output.status.success(), !required, "{}", output.stderr);
        if required {
            assert!(
                output
                    .stderr
                    .contains("host Nix does not honor post-build-hook")
            );
        }
    }
    Ok(())
}

#[test]
fn host_publication_hook_rejects_shared_substitution_and_key_trust_even_with_validation_opt_out()
-> TestResult {
    let host = Host::new()?;
    fs::write(
        host.fixture.workspace.join("wrix.toml"),
        "cache.require_trusted_nix = false\n",
    )?;
    host.start()?;
    let endpoints = host.fixture.endpoints(true)?;
    let state = Path::new(
        endpoints["state_root"]
            .as_str()
            .ok_or("missing state root")?,
    );
    let key = fs::read_to_string(state.join("keys/cache.pub"))?;
    let cache = endpoints["cache_root"]
        .as_str()
        .ok_or("missing cache root")?;
    for configuration in [
        format!("extra-substituters = file://{cache}"),
        format!("extra-trusted-public-keys = {}", key.trim()),
    ] {
        let output = host.activate(&configuration)?;
        assert!(!output.status.success());
        assert!(
            output.stderr.contains("host Nix inherits shared-service"),
            "{}",
            output.stderr
        );
    }
    Ok(())
}

#[test]
fn host_publication_hook_wrapper_delivers_exact_arguments_snapshot_and_private_home() -> TestResult
{
    let host = Host::new()?;
    host.start()?;
    let capture = host.fixture.root.path().join("capture-hook");
    fs::write(
        &capture,
        "#!/usr/bin/env bash\nset -euo pipefail\nprintf '%s\\0' \"$HOME\" \"$XDG_STATE_HOME\" \"$XDG_CACHE_HOME\" \"$@\" >\"$WRIX_HOOK_CAPTURE\"\n",
    )?;
    set_mode(&capture, 0o755)?;
    let output = run_command(
        host.shell("source \"$HOST_SOURCE\"; printf '%s\\n' \"$NIX_CONFIG\"")?
            .env("NIX_CONFIG", "")
            .env("WRIX_CACHE_HOOK_BIN", &capture),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    let wrapper = PathBuf::from(settings(&output.stdout)["post-build-hook"]);
    let physical = host
        .fixture
        .root
        .path()
        .join("isolated-store")
        .join(wrapper.strip_prefix("/")?);
    let captured = host.fixture.root.path().join("captured");
    let execution = run_command(
        host.shell("bash \"$1\"")?
            .arg("wrapper")
            .arg(&physical)
            .env("WRIX_HOOK_CAPTURE", &captured),
    )?;
    assert!(execution.status.success(), "{}", execution.stderr);
    let bytes = fs::read(captured)?;
    let arguments: Vec<OsString> = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| OsString::from(String::from_utf8_lossy(part).into_owned()))
        .collect();
    let root = host.fixture.root.path();
    assert_eq!(arguments[0], root.join("home"));
    assert_eq!(arguments[1], root.join("state"));
    assert_eq!(arguments[2], root.join("cache"));
    let endpoints = host.fixture.endpoints(true)?;
    let uid = run_command(Command::new("id").arg("-u"))?;
    let gid = run_command(Command::new("id").arg("-g"))?;
    assert!(uid.status.success() && gid.status.success());
    let expected = [
        "--workspace-hash",
        endpoints["workspace_hash"]
            .as_str()
            .ok_or("missing workspace hash")?,
        "--owner-uid",
        uid.stdout.trim(),
        "--owner-gid",
        gid.stdout.trim(),
    ];
    assert_eq!(arguments[3..9], expected.map(OsString::from));
    let (argument_pairs, remainder) = arguments[3..].as_chunks::<2>();
    assert_eq!(remainder, &[] as &[OsString]);
    let pairs: BTreeMap<_, _> = argument_pairs
        .iter()
        .map(|pair| (pair[0].to_string_lossy().into_owned(), pair[1].clone()))
        .collect();
    assert_eq!(
        pairs["--state-root"],
        PathBuf::from(endpoints["state_root"].as_str().ok_or("missing state")?)
    );
    assert_eq!(
        pairs["--cache-root"],
        PathBuf::from(endpoints["cache_root"].as_str().ok_or("missing cache")?)
    );
    assert_eq!(
        pairs["--manifest"],
        Path::new(endpoints["state_root"].as_str().ok_or("missing state")?)
            .join("publish-roots.json")
    );
    assert_eq!(pairs["--publisher-helper"], "/bin/true");
    assert!(Path::new(&pairs["--config"]).is_file());
    Ok(())
}
