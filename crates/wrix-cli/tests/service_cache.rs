mod common;

use std::{fs, path::PathBuf, process::Command};

use common::{TestResult, run_command, set_mode, wrix_command_with_path};
use serde_json::{Value, json};

struct Fixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    fn new() -> TestResult<Self> {
        let root = tempfile::Builder::new()
            .prefix("service-cache")
            .tempdir_in("/tmp")?;
        let workspace = root.path().join("workspace");
        let bin = root.path().join("bin");
        fs::create_dir_all(&workspace)?;
        common::run_git(&workspace, &["init", "-q"])?;
        fs::create_dir(&bin)?;
        for runtime in ["podman", "container", "skopeo"] {
            let path = bin.join(runtime);
            fs::write(&path, include_str!("fixtures/service-runtime.sh"))?;
            set_mode(&path, 0o755)?;
        }
        Ok(Self {
            root,
            workspace,
            bin,
        })
    }

    fn command(&self, runtime: &str) -> TestResult<Command> {
        let mut command = wrix_command_with_path(&self.workspace, &[&self.bin])?;
        command
            .env("HOME", self.root.path().join("home"))
            .env("XDG_STATE_HOME", self.root.path().join("state"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .env("WRIX_CONTAINER_RUNTIME", self.bin.join(runtime))
            .env("WRIX_NIX_STORE", common::command_path("nix-store")?)
            .env("WRIX_IMAGE_KEEP_FILE", self.root.path().join("mru.json"))
            .env("WRIX_TEST_CALLS", self.root.path().join("calls"))
            .env("WRIX_TEST_IMAGE", self.root.path().join("image"))
            .env("WRIX_TEST_RUNNING", self.root.path().join("running"))
            .env("WRIX_TEST_RUN_ARGS", self.root.path().join("run.argv"))
            .env("WRIX_TEST_DIGEST", format!("sha256:{}", "a".repeat(64)))
            .env_remove("WRIX_SERVICE_ALLOW_TEMP_CACHE")
            .env_remove("WRIX_DOLT_TRANSPORT")
            .env_remove("WRIX_SERVICE_IMAGE_SOURCE")
            .env_remove("WRIX_SERVICE_IMAGE_SOURCE_KIND")
            .env_remove("WRIX_SERVICE_IMAGE_DIGEST");
        Ok(command)
    }

    fn write_archive(&self, path: &std::path::Path) -> TestResult {
        fs::write(self.root.path().join("config.json"), "{\"config\":{}}")?;
        fs::write(
            self.root.path().join("manifest.json"),
            "[{\"Config\":\"config.json\",\"Layers\":[]}]",
        )?;
        let archive = Command::new(common::command_path("tar")?)
            .current_dir(self.root.path())
            .arg("-cf")
            .arg(path)
            .args(["config.json", "manifest.json"])
            .output()?;
        assert!(archive.status.success());
        Ok(())
    }

    fn endpoints(&self, cache: bool) -> TestResult<Value> {
        let mut command = self.command("podman")?;
        command.args(["service", "endpoints"]);
        if cache {
            command.env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1");
        } else {
            command.arg("--no-cache");
        }
        let output = run_command(&mut command)?;
        assert!(output.status.success(), "{}", output.stderr);
        Ok(serde_json::from_str(&output.stdout)?)
    }
}

#[test]
fn absent_xdg_variables_select_home_based_platform_roots() -> TestResult {
    let fixture = Fixture::new()?;
    let output = run_command(
        fixture
            .command("podman")?
            .args(["service", "endpoints", "--no-cache"])
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CACHE_HOME"),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    let metadata: Value = serde_json::from_str(&output.stdout)?;
    let hash = metadata["workspace_hash"].as_str().unwrap();
    let home = fixture.root.path().join("home");
    let (state, cache) = if cfg!(target_os = "macos") {
        ("Library/Application Support", "Library/Caches")
    } else {
        (".local/state", ".cache")
    };
    let state_root = home.join(state).join("wrix/workspaces").join(hash);
    let cache_root = home
        .join(cache)
        .join("wrix/workspaces")
        .join(hash)
        .join("binary-cache");
    assert_eq!(metadata["state_root"].as_str(), state_root.to_str());
    assert_eq!(metadata["cache_root"].as_str(), cache_root.to_str());
    Ok(())
}

#[test]
fn temp_cache_only_start_never_creates_a_service_container() -> TestResult {
    let fixture = Fixture::new()?;
    let output = run_command(fixture.command("podman")?.args(["service", "start"]))?;
    assert!(output.status.success(), "{}", output.stderr);
    assert!(!fixture.root.path().join("run.argv").exists());
    let endpoints = run_command(fixture.command("podman")?.args(["service", "endpoints"]))?;
    assert!(endpoints.status.success(), "{}", endpoints.stderr);
    let metadata: Value = serde_json::from_str(&endpoints.stdout)?;
    assert!(metadata["endpoints"]["cache_http"].is_null());
    assert!(metadata["endpoints"]["dolt"].is_null());
    assert!(!PathBuf::from(metadata["cache_root"].as_str().unwrap()).exists());
    Ok(())
}

#[test]
fn cache_layout_uses_exact_platform_roots_and_opt_out_creates_no_cache() -> TestResult {
    for cache in [false, true] {
        let fixture = Fixture::new()?;
        let mut command = fixture.command("podman")?;
        command
            .args(["service", "start"])
            .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1");
        if !cache {
            command.arg("--no-cache");
        }
        let output = run_command(&mut command)?;
        assert!(output.status.success(), "{}", output.stderr);
        let metadata = fixture.endpoints(cache)?;
        let hash = metadata["workspace_hash"].as_str().unwrap();
        let home = fixture.root.path().join("home");
        let state_base = if cfg!(target_os = "macos") {
            home.join("Library/Application Support")
        } else {
            fixture.root.path().join("state")
        };
        let cache_base = if cfg!(target_os = "macos") {
            home.join("Library/Caches")
        } else {
            fixture.root.path().join("cache")
        };
        let state_root = state_base.join("wrix/workspaces").join(hash);
        let cache_root = cache_base
            .join("wrix/workspaces")
            .join(hash)
            .join("binary-cache");
        assert_eq!(metadata["state_root"].as_str(), state_root.to_str());
        assert_eq!(metadata["cache_root"].as_str(), cache_root.to_str());
        assert!(!state_root.starts_with(&fixture.workspace));
        assert!(!cache_root.starts_with(&fixture.workspace));
        assert!(state_root.join("services.json").is_file());
        for file in [
            "cache.lock",
            "cache-status.json",
            "keys/cache.secret",
            "keys/cache.pub",
            "publish-roots.json",
        ] {
            assert_eq!(state_root.join(file).is_file(), cache, "{file}");
        }
        for dir in ["gcroots", "pending"] {
            assert_eq!(state_root.join(dir).is_dir(), cache, "{dir}");
        }
        assert_eq!(cache_root.join("nix-cache-info").is_file(), cache);
        assert_eq!(cache_root.join("nar").is_dir(), cache);
        assert_eq!(cache_root.join("log").is_dir(), cache);
        if cache {
            let key = fs::read_to_string(state_root.join("keys/cache.pub"))?;
            wrix_core::cache_key::CachePublicKey::parse(key.trim())?;
            let persisted: Value =
                serde_json::from_slice(&fs::read(state_root.join("services.json"))?)?;
            assert_eq!(persisted, metadata);
            let args = fs::read(fixture.root.path().join("run.argv"))?;
            let mount = format!("{}:/cache:ro", cache_root.display());
            assert!(
                args.split(|byte| *byte == 0)
                    .any(|arg| arg == mount.as_bytes())
            );
        } else {
            assert!(!fixture.root.path().join("run.argv").exists());
        }
    }
    Ok(())
}

#[test]
fn service_start_uses_shared_image_digest_preflight_and_retention() -> TestResult {
    for (runtime, kind) in [
        ("podman", "nix-descriptor"),
        ("container", "docker-archive"),
    ] {
        for installed in [false, true] {
            let fixture = Fixture::new()?;
            let source = fixture.root.path().join("source");
            let layout = fixture.root.path().join("layout");
            let digest = format!("sha256:{}", "a".repeat(64));
            if installed {
                fs::write(fixture.root.path().join("image"), "installed image")?;
            } else if runtime == "podman" {
                fs::create_dir(&layout)?;
                fs::write(
                    &source,
                    serde_json::to_vec(&json!({
                        "schema": 1, "source_kind": kind, "digest": digest,
                        "oci_layout": layout, "oci_ref": "latest", "layers": []
                    }))?,
                )?;
            } else {
                fixture.write_archive(&source)?;
            }
            let output = run_command(
                fixture
                    .command(runtime)?
                    .args(["service", "start"])
                    .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1")
                    .env("WRIX_SERVICE_IMAGE", "wrix-service:test")
                    .env("WRIX_SERVICE_IMAGE_SOURCE", &source)
                    .env("WRIX_SERVICE_IMAGE_SOURCE_KIND", kind)
                    .env("WRIX_SERVICE_IMAGE_DIGEST", &digest),
            )?;
            assert!(output.status.success(), "{}", output.stderr);
            assert_eq!(source.exists(), !installed);
            let calls = String::from_utf8(fs::read(fixture.root.path().join("calls"))?)?;
            let args = calls.split('\0').collect::<Vec<_>>();
            assert_eq!(args.contains(&"copy"), !installed);
            assert_eq!(args.contains(&"load"), !installed && runtime == "container");
            if !installed {
                let source_arg = if runtime == "podman" {
                    format!("oci:{}:latest", layout.display())
                } else {
                    format!("docker-archive:{}", source.display())
                };
                assert!(args.contains(&source_arg.as_str()));
            }
            let records: Value =
                serde_json::from_slice(&fs::read(fixture.root.path().join("mru.json"))?)?;
            assert_eq!(records[0]["ref"], "wrix-service:test");
            assert_eq!(records[0]["digest"], digest);
            let args = fs::read(fixture.root.path().join("run.argv"))?;
            assert!(
                args.split(|byte| *byte == 0)
                    .any(|arg| arg == b"wrix-service:test")
            );
        }
    }
    Ok(())
}

#[test]
fn image_copy_fixture_conforms_to_raw_inspection_and_archive_output() -> TestResult {
    let fixture = Fixture::new()?;
    let environment = fixture.command("container")?;
    let invoke = |program: &str, args: &[&str]| -> TestResult<std::process::Output> {
        Ok(Command::new(fixture.bin.join(program))
            .envs(
                environment
                    .get_envs()
                    .filter_map(|(key, value)| value.map(|value| (key, value))),
            )
            .args(args)
            .output()?)
    };
    let source_path = fixture.root.path().join("fixture.tar");
    fixture.write_archive(&source_path)?;
    let source = format!("docker-archive:{}", source_path.display());
    let inspected = invoke("skopeo", &["inspect", "--raw", &source])?;
    assert!(inspected.status.success());
    let metadata: Value = serde_json::from_slice(&inspected.stdout)?;
    assert_eq!(
        metadata["config"]["digest"],
        format!("sha256:{}", "a".repeat(64))
    );
    let archive = fixture.root.path().join("converted.tar");
    let destination = format!("oci-archive:{}", archive.display());
    assert!(
        invoke(
            "skopeo",
            &[
                "--insecure-policy",
                "copy",
                "--quiet",
                &source,
                &destination
            ]
        )?
        .status
        .success()
    );
    assert!(archive.is_file());
    let loaded = invoke(
        "container",
        &["image", "load", "--input", archive.to_str().unwrap()],
    )?;
    assert!(loaded.status.success());
    assert_eq!(
        String::from_utf8(loaded.stdout)?.trim(),
        format!("Loaded: untagged@sha256:{}", "a".repeat(64))
    );
    assert!(fixture.root.path().join("image").exists());
    assert!(
        !invoke(
            "skopeo",
            &["inspect", "--raw", "docker-archive:/missing/fixture.tar"]
        )?
        .status
        .success()
    );
    assert_eq!(invoke("skopeo", &["unsupported"])?.status.code(), Some(64));
    Ok(())
}

#[test]
fn service_runtime_fixture_conforms_to_missing_running_and_image_contracts() -> TestResult {
    for runtime in ["podman", "container"] {
        let fixture = Fixture::new()?;
        let environment = fixture.command(runtime)?;
        let invoke = |args: &[&str]| -> TestResult<std::process::Output> {
            Ok(Command::new(fixture.bin.join(runtime))
                .envs(
                    environment
                        .get_envs()
                        .filter_map(|(key, value)| value.map(|value| (key, value))),
                )
                .args(args)
                .output()?)
        };
        assert!(
            !invoke(&["image", "inspect", "wrix-service:test"])?
                .status
                .success()
        );
        assert!(!invoke(&["inspect", "workspace-service"])?.status.success());
        fs::write(fixture.root.path().join("image"), "installed image")?;
        let image = invoke(&["image", "inspect", "wrix-service:test"])?;
        assert!(image.status.success());
        let digest = format!("sha256:{}", "a".repeat(64));
        if runtime == "container" {
            let value: Value = serde_json::from_slice(&image.stdout)?;
            assert_eq!(value[0]["digest"], digest);
        } else {
            assert_eq!(String::from_utf8(image.stdout)?.trim(), digest);
        }
        assert!(
            invoke(&["run", "-d", "--name", "workspace-service", "image"])?
                .status
                .success()
        );
        let running = invoke(&["inspect", "workspace-service"])?;
        assert!(running.status.success());
        if runtime == "container" {
            let value: Value = serde_json::from_slice(&running.stdout)?;
            assert_eq!(value[0]["status"]["state"], "running");
        } else {
            assert_eq!(String::from_utf8(running.stdout)?.trim(), "true");
        }
        assert_eq!(invoke(&["unsupported"])?.status.code(), Some(64));
    }
    Ok(())
}
