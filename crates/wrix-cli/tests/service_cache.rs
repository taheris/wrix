#[path = "common/celler.rs"]
mod celler;
mod common;
#[path = "common/service_binding.rs"]
mod service_binding;

use std::{fs, path::PathBuf, process::Command};

use common::{TestResult, run_command, set_mode, wrix_command_with_path};
use serde_json::{Value, json};

struct Fixture {
    server: celler::Server,
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
        service_binding::create(root.path(), &bin, "nix-descriptor")?;
        fs::write(root.path().join("image"), "installed")?;
        let server = celler::Server::watch(root.path(), None);
        common::initialize_host_scope(root.path())?;
        Ok(Self {
            server,
            root,
            workspace,
            bin,
        })
    }

    fn command(&self, runtime: &str) -> TestResult<Command> {
        let mut command = wrix_command_with_path(&self.workspace, &[&self.bin])?;
        command
            .env(
                "WRIX_SERVICE_PACKAGE_BINDING",
                self.root.path().join("binding.json"),
            )
            .env("HOME", self.root.path().join("home"))
            .env("XDG_STATE_HOME", self.root.path().join("state"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .env("WRIX_CONTAINER_RUNTIME", self.bin.join(runtime))
            .env("WRIX_NIX_STORE", common::command_path("nix-store")?)
            .env("WRIX_IMAGE_KEEP_FILE", self.root.path().join("mru.json"))
            .env("WRIX_TEST_CALLS", self.root.path().join("calls"))
            .env("WRIX_TEST_JQ_BIN", common::command_path("jq")?)
            .env("WRIX_TEST_IMAGE", self.root.path().join("image"))
            .env("WRIX_TEST_RUNNING", self.root.path().join("running"))
            .env("WRIX_TEST_RUN_ARGS", self.root.path().join("run.argv"))
            .env("WRIX_TEST_DIGEST", format!("sha256:{}", "a".repeat(64)))
            .env_remove("WRIX_SERVICE_ALLOW_TEMP_CACHE")
            .env_remove("WRIX_DOLT_TRANSPORT")
            .env_remove("WRIX_SERVICE_IMAGE")
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
fn service_fixture_removes_ambient_image_metadata() -> TestResult {
    let fixture = Fixture::new()?;
    let command = fixture.command("podman")?;
    for name in [
        "WRIX_SERVICE_IMAGE",
        "WRIX_SERVICE_IMAGE_SOURCE",
        "WRIX_SERVICE_IMAGE_SOURCE_KIND",
        "WRIX_SERVICE_IMAGE_DIGEST",
    ] {
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == name && value.is_none()),
            "fixture must remove inherited {name}"
        );
    }
    Ok(())
}

#[test]
fn retired_cache_prune_fails_without_state_or_runtime_access() -> TestResult {
    let fixture = Fixture::new()?;
    let before = fixture_snapshot(fixture.root.path())?;
    fs::write(fixture.workspace.join("wrix.toml"), "invalid policy")?;
    for args in [
        vec!["service", "cache", "prune"],
        vec!["service", "cache", "prune", "--host"],
    ] {
        let output = run_command(fixture.command("podman")?.args(args))?;
        assert!(!output.status.success());
        assert!(
            output.stderr.contains("unknown cache command: prune"),
            "{}",
            output.stderr
        );
        assert!(!output.stderr.contains("invalid policy"));
    }
    assert_eq!(fixture_snapshot(fixture.root.path())?, before);
    for path in ["cache", "calls", "run.argv"] {
        assert!(!fixture.root.path().join(path).exists(), "{path}");
    }
    Ok(())
}

fn fixture_snapshot(
    root: &std::path::Path,
) -> TestResult<std::collections::BTreeMap<PathBuf, Option<Vec<u8>>>> {
    fn visit(
        path: &std::path::Path,
        files: &mut std::collections::BTreeMap<PathBuf, Option<Vec<u8>>>,
    ) -> TestResult {
        if path.is_dir() {
            files.insert(path.to_path_buf(), None);
            for entry in fs::read_dir(path)? {
                visit(&entry?.path(), files)?;
            }
        } else {
            files.insert(path.to_path_buf(), Some(fs::read(path)?));
        }
        Ok(())
    }
    let mut files = std::collections::BTreeMap::new();
    for name in ["state", "home"] {
        let path = root.join(name);
        if path.exists() {
            visit(&path, &mut files)?;
        }
    }
    Ok(files)
}

#[test]
fn cache_start_bootstraps_host_private_issuer_without_static_fallback() -> TestResult {
    let fixture = Fixture::new()?;
    let output = run_command(
        fixture
            .command("podman")?
            .args(["service", "start"])
            .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1"),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    let metadata = fixture.endpoints(true)?;
    let state = PathBuf::from(metadata["state_root"].as_str().unwrap());
    let host = state.parent().unwrap().parent().unwrap();
    let issuer = host.join("host-control/celler-issuer");
    let delegated = host.join("service-data/celler");
    wrix_cache::backend::issuer::Authority::open(&issuer)?;
    let private = fs::read(issuer.join("issuer.pem"))?;
    let verifier = fs::read(delegated.join("jwt.pub"))?;
    assert!(String::from_utf8_lossy(&private).contains("PRIVATE KEY"));
    assert!(String::from_utf8_lossy(&verifier).contains("PUBLIC KEY"));
    assert!(!String::from_utf8_lossy(&verifier).contains("PRIVATE KEY"));
    assert!(!delegated.join("issuer.pem").exists());
    let arguments = fs::read(fixture.root.path().join("run.argv"))?;
    assert!(!String::from_utf8_lossy(&arguments).contains(issuer.to_str().unwrap()));
    assert!(!output.stdout.contains("PRIVATE KEY"));
    assert!(!output.stderr.contains("PRIVATE KEY"));
    assert!(!state.join("keys/cache.secret").exists());
    assert!(!PathBuf::from(metadata["cache_root"].as_str().unwrap()).exists());
    let namespace = wrix_cache::backend::publication::registration::private_namespace(
        &wrix_core::path::WorkspaceHash::parse(metadata["workspace_hash"].as_str().unwrap())?,
    )?;
    let binding: Value = serde_json::from_slice(&fs::read(state.join("celler-publisher.json"))?)?;
    assert_eq!(binding["namespace"], namespace.as_str());
    assert_eq!(binding["issuer_root"], issuer.to_str().unwrap());
    assert!(binding.get("token").is_none());
    assert_eq!(
        fs::read_to_string(state.join("keys/cache.pub"))?,
        format!("{}\n", celler::KEY)
    );
    let requests = fs::read_to_string(fixture.root.path().join("celler-requests"))?;
    assert_eq!(
        requests,
        format!(
            "GET /_api/v1/cache-config/{namespace}\nPATCH /_api/v1/cache-config/{namespace}\nGET /_api/v1/cache-config/{namespace}\nGET /_api/v1/cache-config/wrix-common-v1\nGET /_api/v1/cache-config/wrix-common-v1\nPATCH /_api/v1/cache-config/wrix-common-v1\nGET /_api/v1/cache-config/wrix-common-v1\n"
        )
    );
    Ok(())
}

#[test]
fn startup_authentication_failure_does_not_confuse_runtime_liveness_with_readiness() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.path().join("celler-status"), "403")?;
    let output = run_command(
        fixture
            .command("podman")?
            .args(["service", "start"])
            .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1"),
    )?;
    assert!(!output.status.success());
    assert!(output.stderr.contains("HTTP 403"), "{}", output.stderr);
    assert!(!output.stdout.contains("runtime: Running"));
    assert!(fixture.root.path().join("running").exists());
    let metadata = fixture.endpoints(true)?;
    let state = PathBuf::from(metadata["state_root"].as_str().unwrap());
    assert!(!state.join("keys/cache.pub").exists());
    assert!(!state.join("celler-publisher.json").exists());
    let requests = fs::read_to_string(fixture.root.path().join("celler-requests"))?;
    assert_eq!(
        requests.lines().count(),
        1,
        "authentication failure must not retry"
    );
    assert!(!output.stderr.contains("Bearer"));
    Ok(())
}

#[test]
fn confirmed_startup_reports_failed_local_admission_without_rollback_claims() -> TestResult {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new()?;
    let metadata = fixture.endpoints(true)?;
    let state = PathBuf::from(metadata["state_root"].as_str().unwrap());
    wrix_core::private_directory::prepare_with_parents(&state)?;
    common::import_host_namespace(
        fixture.root.path(),
        metadata["workspace_hash"].as_str().unwrap(),
    )?;
    wrix_core::private_directory::prepare_with_parents(&state.join("keys"))?;
    let outside = fixture.root.path().join("outside.pub");
    fs::write(&outside, "preserve external key")?;
    symlink(&outside, state.join("keys/cache.pub"))?;
    let output = run_command(
        fixture
            .command("podman")?
            .args(["service", "start"])
            .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1"),
    )?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("backend changes remain active"),
        "{}",
        output.stderr
    );
    assert!(!state.join("celler-publisher.json").exists());
    assert_eq!(fs::read_to_string(outside)?, "preserve external key");
    assert!(fixture.root.path().join("celler-policy.json").exists());
    assert!(fixture.root.path().join("running").exists());
    Ok(())
}

#[test]
fn cache_start_preserves_conflicting_verifier_without_committing_private_authority() -> TestResult {
    let fixture = Fixture::new()?;
    let metadata = fixture.endpoints(true)?;
    let state = PathBuf::from(metadata["state_root"].as_str().unwrap());
    let host = state.parent().unwrap().parent().unwrap();
    let delegated = host.join("service-data/celler");
    wrix_core::private_directory::prepare_with_parents(&delegated)?;
    let verifier = include_str!("fixtures/celler-verifier.pem");
    fs::write(delegated.join("jwt.pub"), verifier)?;
    let output = run_command(
        fixture
            .command("podman")?
            .args(["service", "start"])
            .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1"),
    )?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("explicit recovery"),
        "{}",
        output.stderr
    );
    assert_eq!(fs::read_to_string(delegated.join("jwt.pub"))?, verifier);
    assert!(!host.join("host-control/celler-issuer/issuer.pem").exists());
    assert!(!delegated.join("server.toml").exists());
    assert!(!fixture.root.path().join("run.argv").exists());
    assert!(!state.join("services.json").exists());
    Ok(())
}

#[test]
fn cache_opt_out_creates_no_issuer_authority() -> TestResult {
    let fixture = Fixture::new()?;
    let output = run_command(
        fixture
            .command("podman")?
            .args(["service", "start", "--no-cache"]),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    let metadata = fixture.endpoints(false)?;
    let state = PathBuf::from(metadata["state_root"].as_str().unwrap());
    let host = state.parent().unwrap().parent().unwrap();
    assert!(!host.join("host-control").exists());
    assert!(!host.join("service-data/celler").exists());
    assert!(!fixture.root.path().join("run.argv").exists());
    Ok(())
}

#[test]
fn service_start_rejects_unbound_ambient_image_substitutes() -> TestResult {
    let fixture = Fixture::new()?;
    let output = run_command(
        fixture
            .command("podman")?
            .args(["service", "start"])
            .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1")
            .env_remove("WRIX_SERVICE_PACKAGE_BINDING")
            .env("WRIX_SERVICE_IMAGE", "project:substitute"),
    )?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("no service-image binding"),
        "{}",
        output.stderr
    );
    assert!(!fixture.root.path().join("calls").exists());
    assert!(!fixture.root.path().join("run.argv").exists());
    Ok(())
}

#[test]
fn service_start_rejects_conflicting_ambient_image_before_runtime_access() -> TestResult {
    let fixture = Fixture::new()?;
    let output = run_command(
        fixture
            .command("podman")?
            .args(["service", "start"])
            .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1")
            .env("WRIX_SERVICE_IMAGE", "project:substitute"),
    )?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("conflicts with"),
        "{}",
        output.stderr
    );
    assert!(!fixture.root.path().join("calls").exists());
    Ok(())
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
fn repository_cache_opt_out_suppresses_startup_and_environment_can_override_it() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(fixture.workspace.join("wrix.toml"), "cache.enable = false")?;
    let output = run_command(
        fixture
            .command("podman")?
            .args(["service", "start"])
            .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1"),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    assert!(!fixture.root.path().join("run.argv").exists());
    let metadata = fixture.endpoints(true)?;
    assert!(metadata["endpoints"]["cache_http"].is_null());
    assert!(!PathBuf::from(metadata["cache_root"].as_str().unwrap()).exists());

    let mut command = fixture.command("podman")?;
    command
        .args(["service", "endpoints"])
        .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1")
        .env("WRIX_NIX_CACHE_ENABLE", "1");
    let output = run_command(&mut command)?;
    assert!(output.status.success(), "{}", output.stderr);
    let metadata: Value = serde_json::from_str(&output.stdout)?;
    assert!(!metadata["endpoints"]["cache_http"].is_null());

    command.arg("--no-cache");
    let output = run_command(&mut command)?;
    assert!(output.status.success(), "{}", output.stderr);
    let metadata: Value = serde_json::from_str(&output.stdout)?;
    assert!(metadata["endpoints"]["cache_http"].is_null());
    Ok(())
}

#[test]
fn celler_layout_separates_managed_cache_data_from_host_authority() -> TestResult {
    for cache in [false, true] {
        let fixture = Fixture::new()?;
        if cache {
            service_binding::provision_celler_authority(fixture.root.path())?;
        }
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
        let observer = std::path::Path::new("/etc/wrix/image-agent").exists();
        assert_eq!(state_root.join("services.json").is_file(), observer);
        for file in ["cache-status.json", "publish-roots.json"] {
            assert_eq!(state_root.join(file).is_file(), cache && observer, "{file}");
        }
        assert_eq!(state_root.join("cache.lock").is_file(), cache);
        for dir in ["gcroots", "pending"] {
            assert_eq!(state_root.join(dir).is_dir(), cache && observer, "{dir}");
        }
        assert!(!cache_root.exists());
        assert!(!state_root.join("keys/cache.secret").exists());
        assert_eq!(state_root.join("keys/cache.pub").exists(), cache);
        if cache {
            let key = fs::read_to_string(state_root.join("keys/cache.pub"))?;
            wrix_core::cache_key::CachePublicKey::parse(key.trim())?;
            let persisted: Value = serde_json::from_slice(&common::read_namespace_document(
                &state_root.join("services.json"),
            )?)?;
            assert_eq!(persisted, metadata);
            let args = fs::read(fixture.root.path().join("run.argv"))?;
            let layout = wrix_cache::backend::layout::Layout::resolve(
                if cfg!(target_os = "macos") {
                    wrix_cache::backend::layout::Platform::Darwin
                } else {
                    wrix_cache::backend::layout::Platform::Linux
                },
                &home,
                Some(&state_base),
                Some(&cache_base),
            )?;
            for mount in layout.mounts() {
                let value = format!("{}:{}:rw", mount.source.display(), mount.destination);
                assert!(
                    args.split(|byte| *byte == 0)
                        .any(|arg| arg == value.as_bytes())
                );
            }
            assert!(
                !args
                    .split(|byte| *byte == 0)
                    .any(|arg| arg.ends_with(b":/cache:ro"))
            );
        } else {
            assert!(!fixture.root.path().join("run.argv").exists());
        }
    }
    Ok(())
}

#[test]
fn enabled_service_prepares_only_private_shared_celler_data_and_opt_out_does_not() -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;

    for cache in [false, true] {
        let fixture = Fixture::new()?;
        let home = fixture.root.path().join("home");
        let layout = wrix_cache::backend::layout::Layout::resolve(
            if cfg!(target_os = "macos") {
                wrix_cache::backend::layout::Platform::Darwin
            } else {
                wrix_cache::backend::layout::Platform::Linux
            },
            &home,
            Some(&fixture.root.path().join("state")),
            Some(&fixture.root.path().join("cache")),
        )?;
        fixture.endpoints(cache)?;
        assert!(!layout.metadata().exists());
        assert!(!layout.archives().exists());
        if cache {
            service_binding::provision_celler_authority(fixture.root.path())?;
        }
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
        let state = PathBuf::from(metadata["state_root"].as_str().unwrap());
        assert_eq!(fs::metadata(&state)?.permissions().mode() & 0o777, 0o700);
        for mount in layout.mounts() {
            assert_eq!(mount.source.is_dir(), cache);
            assert!(!mount.source.starts_with(&fixture.workspace));
            assert!(!mount.source.starts_with(&state));
            if cache {
                assert_eq!(
                    fs::metadata(mount.source)?.permissions().mode() & 0o777,
                    0o700
                );
                assert!(!mount.source.join("celler-publisher.json").exists());
                assert!(!mount.source.join("publish-roots.json").exists());
                assert!(!mount.source.join("cache.secret").exists());
            }
        }
    }
    Ok(())
}

#[test]
fn service_reentry_tightens_legacy_control_without_losing_runtime_authority() -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = Fixture::new()?;
    let metadata = fixture.endpoints(false)?;
    let state = PathBuf::from(metadata["state_root"].as_str().unwrap());
    wrix_core::private_directory::prepare_with_parents(&state)?;
    common::import_host_namespace(
        fixture.root.path(),
        metadata["workspace_hash"].as_str().unwrap(),
    )?;
    set_mode(&state, 0o755)?;
    let binding = json!({
        "schema_version": 1,
        "workspace_hash": metadata["workspace_hash"],
        "endpoint": "http://127.0.0.1:8080/",
        "namespace": "private",
        "token": "disposable-runtime-authority-fixture",
        "retention_seconds": 2_592_000
    });
    let bytes = serde_json::to_vec(&binding)?;
    let path = state.join("celler-publisher.json");
    fs::write(&path, &bytes)?;
    set_mode(&path, 0o600)?;
    fs::write(state.join("publish-roots.json"), "preserved manifest")?;
    let output = run_command(
        fixture
            .command("podman")?
            .args(["service", "start", "--no-cache"]),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    assert_eq!(fs::metadata(&state)?.permissions().mode() & 0o777, 0o700);
    assert_eq!(fs::read(&path)?, bytes);
    assert_eq!(
        fs::read_to_string(state.join("publish-roots.json"))?,
        "preserved manifest"
    );
    let workspace =
        wrix_core::path::WorkspaceHash::parse(metadata["workspace_hash"].as_str().unwrap())?;
    wrix_cache::backend::publication::Publisher::load(&state, &workspace)?;
    assert!(
        !output
            .stdout
            .contains("disposable-runtime-authority-fixture")
    );
    assert!(
        !output
            .stderr
            .contains("disposable-runtime-authority-fixture")
    );
    Ok(())
}

#[test]
fn service_rejects_symlinked_control_before_lock_or_runtime_access() -> TestResult {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let fixture = Fixture::new()?;
    let metadata = fixture.endpoints(false)?;
    let state = PathBuf::from(metadata["state_root"].as_str().unwrap());
    fs::create_dir_all(state.parent().unwrap())?;
    let unrelated = fixture.root.path().join("unrelated");
    fs::create_dir(&unrelated)?;
    set_mode(&unrelated, 0o755)?;
    fs::write(unrelated.join("preserved"), "unrelated data")?;
    symlink(&unrelated, &state)?;
    let output = run_command(
        fixture
            .command("podman")?
            .args(["service", "start", "--no-cache"]),
    )?;
    assert!(!output.status.success());
    assert!(!unrelated.join("service.lock").exists());
    assert!(!unrelated.join("services.json").exists());
    assert_eq!(
        fs::metadata(&unrelated)?.permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::read_to_string(unrelated.join("preserved"))?,
        "unrelated data"
    );
    assert!(!fixture.root.path().join("calls").exists());
    Ok(())
}

#[test]
fn service_startup_does_not_collect_dirty_cache_contents() -> TestResult {
    for elapsed in [3600, 172_800] {
        let fixture = Fixture::new()?;
        service_binding::provision_celler_authority(fixture.root.path())?;
        let mut command = fixture.command("podman")?;
        command
            .args(["service", "start"])
            .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1");
        let output = run_command(&mut command)?;
        assert!(output.status.success(), "{}", output.stderr);
        let metadata = fixture.endpoints(true)?;
        let state_root = PathBuf::from(metadata["state_root"].as_str().unwrap());
        let cache_root = PathBuf::from(metadata["cache_root"].as_str().unwrap());
        let stale = cache_root.join("nar/orphan.nar");
        fs::create_dir_all(stale.parent().unwrap())?;
        fs::write(&stale, "orphan")?;
        let last_prune = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs()
            .saturating_sub(elapsed);
        common::write_namespace_document(
            &state_root.join("cache-status.json"),
            &serde_json::to_vec(&json!({
                "dirty": true, "last_publish": null, "last_prune": null,
                "last_error": null, "last_prune_epoch": last_prune,
            }))?,
        )?;
        let output = run_command(&mut command)?;
        assert!(output.status.success(), "{}", output.stderr);
        assert!(stale.exists(), "{elapsed}");
        fs::remove_file(fixture.root.path().join("running"))?;
        let output = run_command(&mut command)?;
        assert!(!output.status.success());
        assert!(
            output.stderr.contains("is Missing") && output.stderr.contains("wrix service recover"),
            "{}",
            output.stderr
        );
        assert!(stale.exists(), "{elapsed}");
        let status: Value = serde_json::from_slice(&common::read_namespace_document(
            &state_root.join("cache-status.json"),
        )?)?;
        assert_eq!(status["last_prune_epoch"], last_prune);
        assert_eq!(status["dirty"], true);
    }
    Ok(())
}

#[test]
fn service_start_uses_shared_image_digest_preflight_without_pruning() -> TestResult {
    for (runtime, kind) in [
        ("podman", "nix-descriptor"),
        ("container", "docker-archive"),
    ] {
        for installed in [false, true] {
            let fixture = Fixture::new()?;
            service_binding::provision_celler_authority(fixture.root.path())?;
            let source = fixture.root.path().join("source");
            let layout = fixture.root.path().join("layout");
            let digest = format!("sha256:{}", "a".repeat(64));
            if installed {
                fs::write(fixture.root.path().join("image"), "installed image")?;
            }
            if runtime == "podman" {
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
            let digest_path = fixture.root.path().join("digest");
            fs::write(&digest_path, &digest)?;
            service_binding::write(
                fixture.root.path(),
                &fixture.bin,
                kind,
                &source,
                &digest_path,
            )?;
            if !installed {
                fs::remove_file(fixture.root.path().join("image"))?;
            }
            let output = run_command(
                fixture
                    .command(runtime)?
                    .args(["service", "start"])
                    .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1")
                    .env(
                        "WRIX_SERVICE_PACKAGE_BINDING",
                        fixture.root.path().join("binding.json"),
                    ),
            )?;
            assert!(output.status.success(), "{}", output.stderr);
            assert!(source.exists());
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
            assert!(!fixture.root.path().join("mru.json").exists());
            assert!(!args.contains(&"images"));
            assert!(!args.contains(&"rmi"));
            assert_eq!(
                args.iter().filter(|arg| **arg == "delete").count(),
                usize::from(!installed && runtime == "container")
            );
            let args = fs::read(fixture.root.path().join("run.argv"))?;
            let target = if runtime == "podman" {
                digest.as_bytes()
            } else {
                b"wrix-service:test"
            };
            assert!(args.split(|byte| *byte == 0).any(|arg| arg == target));
            if runtime == "podman" {
                assert!(
                    args.split(|byte| *byte == 0)
                        .any(|arg| arg == b"--pull=never")
                );
                assert!(
                    !args
                        .split(|byte| *byte == 0)
                        .any(|arg| arg == b"wrix-service:test")
                );
            }
        }
    }
    Ok(())
}

#[test]
fn selected_owner_preserves_its_record_when_the_callers_package_changes() -> TestResult {
    use std::os::unix::fs::MetadataExt;
    for (runtime, kind) in [
        ("podman", "nix-descriptor"),
        ("container", "docker-archive"),
    ] {
        let fixture = Fixture::new()?;
        service_binding::create(fixture.root.path(), &fixture.bin, kind)?;
        let mut command = fixture.command(runtime)?;
        command
            .args(["service", "start"])
            .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1");
        let output = run_command(&mut command)?;
        assert!(output.status.success(), "{}", output.stderr);
        let metadata = run_command(
            fixture
                .command(runtime)?
                .args(["service", "endpoints"])
                .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1"),
        )?;
        assert!(metadata.status.success(), "{}", metadata.stderr);
        let metadata: Value = serde_json::from_str(&metadata.stdout)?;
        let state = PathBuf::from(metadata["state_root"].as_str().unwrap());
        let selection = state
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("host-lifecycle/celler-owner.json");
        let before = fs::read(&selection)?;
        let inode = fs::metadata(&selection)?.ino();
        let key = fs::read(state.join("keys/cache.pub"))?;
        let selected: Value = serde_json::from_slice(&before)?;
        if runtime == "podman" {
            assert_eq!(selected["schema_version"], 2);
            assert_eq!(selected["recipe"]["binding"]["image"], "wrix-service:test");
            assert_eq!(
                selected["recipe"]["image_id"],
                format!("sha256:{}", "a".repeat(64))
            );
        } else {
            assert_eq!(selected["schema_version"], 1);
            assert!(selected.get("recipe").is_none());
        }
        let binding = fixture.root.path().join("binding.json");
        let mut caller: Value = serde_json::from_slice(&fs::read(&binding)?)?;
        caller["image"] = json!("wrix-service:new-package");
        fs::write(&binding, serde_json::to_vec(&caller)?)?;
        fs::write(fixture.root.path().join("calls"), "")?;
        let output = run_command(&mut command)?;
        assert!(output.status.success(), "{}", output.stderr);
        let calls = fs::read(fixture.root.path().join("calls"))?;
        let calls: Vec<_> = calls.split(|byte| *byte == 0).collect();
        assert!(!calls.contains(&b"image".as_slice()));
        assert!(!calls.contains(&b"run".as_slice()));
        assert_eq!(fs::read(&selection)?, before);
        assert_eq!(fs::metadata(&selection)?.ino(), inode);
        assert_eq!(fs::read(state.join("keys/cache.pub"))?, key);
        let args = fs::read(fixture.root.path().join("run.argv"))?;
        let mut args: Vec<_> = args.split(|byte| *byte == 0).map(<[u8]>::to_vec).collect();
        let image = args.len() - 5;
        if runtime == "container" {
            assert_eq!(args[image], b"wrix-service:test");
            continue;
        }
        assert_eq!(args[image], format!("sha256:{}", "a".repeat(64)).as_bytes());
        args[image] = format!("sha256:{}", "b".repeat(64)).into_bytes();
        let changed = args.join(&0);
        fs::write(fixture.root.path().join("run.argv"), &changed)?;
        let output = run_command(&mut command)?;
        assert!(!output.status.success());
        assert!(output.stderr.contains("incompatible"), "{}", output.stderr);
        assert_eq!(fs::read(fixture.root.path().join("run.argv"))?, changed);
        assert_eq!(fs::read(&selection)?, before);
        assert_eq!(fs::read(state.join("keys/cache.pub"))?, key);
    }
    Ok(())
}

#[test]
fn podman_recovers_a_missing_legacy_selected_owner_and_records_new_provenance() -> TestResult {
    let mut fixture = Fixture::new()?;
    let output = run_command(
        fixture
            .command("podman")?
            .args(["service", "start"])
            .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1"),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    let metadata = fixture.endpoints(true)?;
    let state = PathBuf::from(metadata["state_root"].as_str().unwrap());
    let selection = state
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("host-lifecycle/celler-owner.json");
    let mut legacy: Value = serde_json::from_slice(&fs::read(&selection)?)?;
    legacy["schema_version"] = json!(1);
    legacy.as_object_mut().unwrap().remove("recipe");
    fs::write(&selection, serde_json::to_vec(&legacy)?)?;
    fs::remove_file(fixture.root.path().join("running"))?;
    drop(fixture.server);
    fs::remove_file(fixture.root.path().join("run.argv"))?;
    fixture.server = celler::Server::watch(fixture.root.path(), None);
    let output = run_command(
        fixture
            .command("podman")?
            .args(["service", "recover"])
            .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1"),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    let recovered: Value = serde_json::from_slice(&fs::read(&selection)?)?;
    assert_eq!(recovered["schema_version"], 2);
    assert_eq!(
        recovered["recipe"]["image_id"],
        format!("sha256:{}", "a".repeat(64))
    );
    for field in [
        "runtime",
        "workspace_hash",
        "container_name",
        "cache_http_port",
    ] {
        assert_eq!(legacy[field], recovered[field], "{field}");
    }
    let output = run_command(
        fixture
            .command("podman")?
            .args(["service", "start"])
            .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1"),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    Ok(())
}

#[test]
fn wrong_installed_podman_config_identity_is_rejected_before_starting_an_owner() -> TestResult {
    let fixture = Fixture::new()?;
    service_binding::create(fixture.root.path(), &fixture.bin, "nix-descriptor")?;
    let output = run_command(
        fixture
            .command("podman")?
            .args(["service", "start"])
            .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1")
            .env("WRIX_TEST_IMAGE_ID", format!("sha256:{}", "b".repeat(64))),
    )?;
    assert!(!output.status.success());
    assert!(
        output
            .stderr
            .contains("does not match the bound content digest"),
        "{}",
        output.stderr
    );
    assert!(!fixture.root.path().join("run.argv").exists());
    let host = if cfg!(target_os = "macos") {
        fixture
            .root
            .path()
            .join("home/Library/Application Support/wrix")
    } else {
        fixture.root.path().join("state/wrix")
    };
    assert!(!host.join("host-lifecycle/celler-owner.json").exists());
    Ok(())
}

#[test]
fn podman_pins_the_config_identity_not_the_registry_manifest_digest() -> TestResult {
    let fixture = Fixture::new()?;
    service_binding::create(fixture.root.path(), &fixture.bin, "nix-descriptor")?;
    let output = run_command(
        fixture
            .command("podman")?
            .args(["service", "start"])
            .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1")
            .env(
                "WRIX_TEST_CONTENT_DIGEST",
                format!("sha256:{}", "b".repeat(64)),
            ),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    let args = fs::read(fixture.root.path().join("run.argv"))?;
    assert!(
        args.split(|byte| *byte == 0)
            .any(|arg| arg == format!("sha256:{}", "a".repeat(64)).as_bytes())
    );
    assert!(
        args.split(|byte| *byte == 0)
            .any(|arg| arg == b"--pull=never")
    );
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
    assert_eq!(
        invoke(
            "container",
            &[
                "image",
                "inspect",
                "-q",
                &format!("sha256:{}", "a".repeat(64))
            ]
        )?
        .status
        .code(),
        Some(64)
    );
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
        fs::remove_file(fixture.root.path().join("image"))?;
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
        let image_inspect: &[&str] = if runtime == "container" {
            &["image", "inspect", "wrix-service:test"]
        } else {
            &[
                "image",
                "inspect",
                "--format",
                "{{.Id}}",
                "wrix-service:test",
            ]
        };
        assert!(!invoke(image_inspect)?.status.success());
        assert!(!invoke(&["inspect", "workspace-service"])?.status.success());
        if runtime == "container" {
            let output = invoke(&["image", "list", "--format", "json"])?;
            assert!(output.status.success());
            assert_eq!(serde_json::from_slice::<Value>(&output.stdout)?, json!([]));
        }
        fs::write(fixture.root.path().join("image"), "installed image")?;
        let image = invoke(image_inspect)?;
        assert!(image.status.success());
        let digest = format!("sha256:{}", "a".repeat(64));
        if runtime == "container" {
            let value: Value = serde_json::from_slice(&image.stdout)?;
            assert_eq!(value[0]["digest"], digest);
            let output = invoke(&["image", "list", "--format", "json"])?;
            assert!(output.status.success());
            let listed: Value = serde_json::from_slice(&output.stdout)?;
            assert_eq!(listed[0]["configuration"]["name"], "wrix-service:test");
            assert_eq!(listed[0]["id"], digest);
        } else {
            assert_eq!(String::from_utf8(image.stdout)?.trim(), digest);
        }
        assert!(
            invoke(&[
                "run",
                "-d",
                "--name",
                "workspace-service",
                "--label",
                "wrix.kind=service",
                "-p",
                "127.0.0.1:21234:8080",
                "image",
            ])?
            .status
            .success()
        );
        let running = if runtime == "container" {
            invoke(&["inspect", "workspace-service"])?
        } else {
            invoke(&[
                "inspect",
                "--format",
                "{{.State.Running}}",
                "workspace-service",
            ])?
        };
        assert!(running.status.success());
        if runtime == "container" {
            let value: Value = serde_json::from_slice(&running.stdout)?;
            assert_eq!(value[0]["status"]["state"], "running");
            assert_eq!(value[0]["configuration"]["labels"]["wrix.kind"], "service");
            assert_eq!(
                value[0]["configuration"]["publishedPorts"][0]["hostPort"],
                21_234
            );
        } else {
            assert_eq!(String::from_utf8(running.stdout)?.trim(), "true");
            let label = invoke(&[
                "inspect",
                "--format",
                "{{ index .Config.Labels \"wrix.kind\" }}",
                "workspace-service",
            ])?;
            assert!(label.status.success());
            assert_eq!(String::from_utf8(label.stdout)?.trim(), "service");
            let ports = invoke(&["port", "workspace-service"])?;
            assert!(ports.status.success());
            assert_eq!(
                String::from_utf8(ports.stdout)?.trim(),
                "8080/tcp -> 127.0.0.1:21234"
            );
        }
        let alternate = if runtime == "podman" {
            "container"
        } else {
            "podman"
        };
        let other = Command::new(fixture.bin.join(alternate))
            .envs(
                environment
                    .get_envs()
                    .filter_map(|(key, value)| value.map(|value| (key, value))),
            )
            .args(["inspect", "workspace-service"])
            .output()?;
        assert!(!other.status.success());
        assert!(String::from_utf8(other.stderr)?.contains("no such object"));
        assert_eq!(invoke(&["unsupported"])?.status.code(), Some(64));
    }
    Ok(())
}
