#![cfg(test)]

use std::{
    env, fs,
    fs::OpenOptions,
    io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use attic::cache::CacheName;
use fs2::FileExt;
use serde_json::{Value, json};
use wrix_cache::{
    backend::publication::Publisher,
    config::Settings,
    publisher::{self, HookRecord, Mode},
};
use wrix_core::path::Workspace;

#[path = "common/celler/mod.rs"]
#[expect(
    dead_code,
    reason = "shared native fixture also exposes corruption and pull-only clients for backend regressions"
)]
mod native;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Fixture {
    root: tempfile::TempDir,
    server: native::Server,
    workspace: Workspace,
    state: PathBuf,
    cache: PathBuf,
    output: PathBuf,
    dependency: PathBuf,
    foreign: PathBuf,
    roots: PathBuf,
    nix: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace_path = root.path().join("workspace");
        fs::create_dir(&workspace_path).unwrap();
        let workspace = Workspace::from_path(&workspace_path).unwrap();
        let dependency = native::add_file("dependency", "dependency");
        let foreign = native::add_file("unrelated", "foreign");
        let eval = Command::new("nix")
            .args(["--store", "daemon", "eval", "--raw", "--impure", "--expr"])
            .arg(format!(
                "builtins.toFile \"selected\" (builtins.storePath {:?})",
                dependency.to_str().unwrap()
            ))
            .output()
            .unwrap();
        assert!(eval.status.success(), "{:?}", eval.stderr);
        let output = PathBuf::from(String::from_utf8(eval.stdout).unwrap().trim());
        let state = root.path().join("state");
        let server = native::Server::start();
        server
            .client("private")
            .register(std::time::Duration::from_hours(720))
            .await
            .unwrap();
        Publisher::install(
            &state,
            workspace.hash(),
            &server.endpoint,
            CacheName::new(String::from("private")).unwrap(),
            &server.token,
            std::time::Duration::from_hours(720),
        )
        .unwrap();
        let nix = root.path().join("nix");
        fs::write(&nix, NIX_FIXTURE).unwrap();
        fs::set_permissions(&nix, fs::Permissions::from_mode(0o755)).unwrap();
        let fixture = Self {
            cache: root.path().join("unused-flat-cache"),
            roots: root.path().join("roots.json"),
            root,
            server,
            workspace,
            state,
            output,
            dependency,
            foreign,
            nix,
        };
        fixture.set_roots(&[fixture.package()]);
        fixture
    }

    fn package(&self) -> Value {
        Self::root("packages.demo", &self.output)
    }

    fn root(name: &str, path: &Path) -> Value {
        json!({"name":name,"installable":format!(".#{name}"),"drv_path":format!("/nix/store/{name}.drv"),"out_paths":[path]})
    }

    fn set_roots(&self, roots: &[Value]) {
        fs::write(&self.roots, json!({"roots":roots}).to_string()).unwrap();
    }

    fn pending(&self, name: &str, drv: &str, path: &Path) -> PathBuf {
        fs::create_dir_all(self.state.join("pending")).unwrap();
        let record = self.state.join(format!("pending/{name}.json"));
        fs::write(
            &record,
            json!({"drv_path":drv,"out_paths":[path]}).to_string(),
        )
        .unwrap();
        record
    }

    fn command(&self, action: &str) -> Command {
        let mut command = Command::new(env::current_exe().unwrap());
        command
            .args(["--ignored", "--exact", "publisher_child", "--nocapture"])
            .current_dir(self.workspace.canonical_path())
            .env("WRIX_CACHE_TEST_ACTION", action)
            .env("WRIX_CACHE_TEST_STATE", &self.state)
            .env("WRIX_CACHE_TEST_CACHE", &self.cache)
            .env("WRIX_CACHE_TEST_OUTPUT", self.root.path().join("report"))
            .env("WRIX_CACHE_TEST_DRV", "/nix/store/packages.demo.drv")
            .env("WRIX_CACHE_TEST_OUT", &self.output)
            .env("WRIX_CACHE_ROOTS_FILE", &self.roots)
            .env("WRIX_NIX_BIN", &self.nix)
            .env("WRIX_NIX_STORE_BIN", "/must-not-use-cli-store-metadata")
            .env("WRIX_TEST_NIX_LOG", self.root.path().join("nix.log"))
            .env("WRIX_UPSTREAM_SUBSTITUTERS", "https://fixture.invalid")
            .env("WRIX_TEST_UPSTREAM", "normal")
            .env("WRIX_TEST_UPSTREAM_HIT", &self.dependency);
        command
    }

    fn run(&self, action: &str, environment: &[(&str, &str)]) -> String {
        let mut command = self.command(action);
        for (name, value) in environment {
            command.env(name, value);
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        fs::read_to_string(self.root.path().join("report")).unwrap()
    }

    fn failure(&self, action: &str, environment: &[(&str, &str)]) -> String {
        let mut command = self.command(action);
        for (name, value) in environment {
            command.env(name, value);
        }
        let result = command.output().unwrap();
        assert!(!result.status.success(), "unexpected success");
        format!(
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        )
    }

    async fn present(&self, path: &Path) -> bool {
        let status = self
            .server
            .get(&format!("private/{}.narinfo", native::hash(path)))
            .await
            .status();
        assert!(
            status == 200 || status == 404,
            "unexpected response {status}"
        );
        status == 200
    }

    fn log(&self) -> String {
        fs::read_to_string(self.root.path().join("nix.log")).unwrap()
    }

    fn policy(&self, policy: &str) {
        fs::write(self.workspace.canonical_path().join("wrix.toml"), policy).unwrap();
    }
}

#[test]
#[ignore = "child helper receives isolated native store and host authority"]
fn publisher_child() -> TestResult {
    let workspace = Workspace::from_path(env::current_dir()?)?;
    let state = PathBuf::from(env::var_os("WRIX_CACHE_TEST_STATE").unwrap());
    let cache = PathBuf::from(env::var_os("WRIX_CACHE_TEST_CACHE").unwrap());
    let report = match env::var("WRIX_CACHE_TEST_ACTION")?.as_str() {
        "auto" => publisher::run_hook_record(
            HookRecord {
                workspace_hash: workspace.hash(),
                state_root: &state,
                cache_root: &cache,
                manifest: Path::new(&env::var_os("WRIX_CACHE_ROOTS_FILE").unwrap()),
                drv_path: &env::var("WRIX_CACHE_TEST_DRV")?,
                out_paths: &env::var("WRIX_CACHE_TEST_OUT")?,
            },
            Settings::for_workspace(&workspace)?,
        )?,
        "status" => publisher::status_at(&state, &cache, Settings::for_workspace(&workspace)?)?,
        "publish" => publisher::run_workspace_at(&workspace, &state, &cache, Mode::Publish)?,
        "warm" => publisher::run_workspace_at(
            &workspace,
            &state,
            &cache,
            Mode::Warm {
                checks: env::var("WRIX_CACHE_TEST_CHECKS").is_ok_and(|value| value == "1"),
            },
        )?,
        other => return Err(io::Error::other(format!("unknown action {other}")).into()),
    };
    fs::write(
        PathBuf::from(env::var_os("WRIX_CACHE_TEST_OUTPUT").unwrap()),
        report.lines().join("\n"),
    )?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn publish_realized_roots_drains_pending_without_flat_state() {
    if !native::in_isolated_store("publish_realized_roots_drains_pending_without_flat_state") {
        return;
    }
    let fixture = Fixture::new().await;
    let record = fixture.pending("matching", "/nix/store/packages.demo.drv", &fixture.foreign);
    let key = fixture.server.client("private").public_key().await.unwrap();
    let report = fixture.run("publish", &[]);
    assert!(report.contains("drained pending:"));
    assert!(!record.exists());
    assert!(fixture.present(&fixture.output).await);
    assert!(fixture.present(&fixture.foreign).await);
    assert!(!fixture.present(&fixture.dependency).await);
    assert_eq!(
        fixture.server.client("private").public_key().await.unwrap(),
        key
    );
    assert!(!fixture.cache.exists());
    assert!(!fixture.state.join("keys/cache.secret").exists());
    assert!(!fixture.state.join("gcroots").exists());
    let manifest: Value =
        serde_json::from_slice(&fs::read(fixture.state.join("publish-roots.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["schema_version"], 1);
    assert_eq!(
        manifest["workspace_hash"],
        fixture.workspace.hash().as_str()
    );
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn publish_filters_to_current_workspace_closure() {
    if !native::in_isolated_store("publish_filters_to_current_workspace_closure") {
        return;
    }
    let fixture = Fixture::new().await;
    let foreign = fixture.pending("foreign", "/nix/store/other.drv", &fixture.foreign);
    fixture.run("publish", &[]);
    assert!(fixture.present(&fixture.output).await);
    assert!(!fixture.present(&fixture.foreign).await);
    assert!(!fixture.present(&fixture.dependency).await);
    assert!(foreign.exists());
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn publish_batches_upstream_queries_and_retains_null_misses() {
    if !native::in_isolated_store("publish_batches_upstream_queries_and_retains_null_misses") {
        return;
    }
    let fixture = Fixture::new().await;
    let paths = (0..260)
        .map(|index| native::add_file(&format!("value {index}"), &format!("candidate-{index}")))
        .collect::<Vec<_>>();
    fixture.set_roots(
        &paths
            .iter()
            .enumerate()
            .map(|(i, path)| Fixture::root(&format!("packages.p{i}"), path))
            .collect::<Vec<_>>(),
    );
    fixture.run("publish", &[]);
    let log = fixture.log();
    let queries = log
        .lines()
        .filter(|line| line.starts_with("path-info --store"))
        .collect::<Vec<_>>();
    assert_eq!(queries.len(), 2);
    for query in queries {
        assert!(query.split_whitespace().count() - 4 <= 256);
    }
    for path in paths {
        assert!(fixture.present(&path).await);
    }
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn publish_uses_successful_upstream_entries_even_on_partial_query_failure() {
    if !native::in_isolated_store(
        "publish_uses_successful_upstream_entries_even_on_partial_query_failure",
    ) {
        return;
    }
    let fixture = Fixture::new().await;
    fixture.run("publish", &[("WRIX_TEST_UPSTREAM", "partial")]);
    assert!(fixture.present(&fixture.output).await);
    assert!(!fixture.present(&fixture.dependency).await);
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn publish_retains_candidates_when_upstream_is_unavailable() {
    if !native::in_isolated_store("publish_retains_candidates_when_upstream_is_unavailable") {
        return;
    }
    let fixture = Fixture::new().await;
    fixture.run("publish", &[("WRIX_TEST_UPSTREAM", "unavailable")]);
    assert!(fixture.present(&fixture.output).await);
    assert!(fixture.present(&fixture.dependency).await);
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn failed_publication_keeps_matching_pending_until_successful_retry() {
    if !native::in_isolated_store(
        "failed_publication_keeps_matching_pending_until_successful_retry",
    ) {
        return;
    }
    let fixture = Fixture::new().await;
    let record = fixture.pending("matching", "/nix/store/packages.demo.drv", &fixture.foreign);
    fixture.failure("publish", &[("WRIX_TEST_UPSTREAM", "malformed")]);
    assert!(record.exists());
    assert!(!fixture.present(&fixture.output).await);
    Publisher::install(
        &fixture.state,
        fixture.workspace.hash(),
        &fixture.server.endpoint,
        CacheName::new(String::from("private")).unwrap(),
        &native::pull_token(),
        std::time::Duration::from_hours(720),
    )
    .unwrap();
    let error = fixture.failure("publish", &[]);
    assert!(!error.contains(&native::pull_token()));
    assert!(record.exists());
    Publisher::install(
        &fixture.state,
        fixture.workspace.hash(),
        &fixture.server.endpoint,
        CacheName::new(String::from("private")).unwrap(),
        &fixture.server.token,
        std::time::Duration::from_hours(720),
    )
    .unwrap();
    fixture.run("publish", &[]);
    assert!(!record.exists());
    assert!(fixture.present(&fixture.foreign).await);
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn warm_roots_include_checks_only_when_requested() {
    if !native::in_isolated_store("warm_roots_include_checks_only_when_requested") {
        return;
    }
    let fixture = Fixture::new().await;
    fixture.set_roots(&[
        fixture.package(),
        Fixture::root("checks.unit", &fixture.foreign),
    ]);
    fixture.run("warm", &[]);
    assert!(fixture.present(&fixture.output).await);
    assert!(!fixture.present(&fixture.foreign).await);
    assert!(!fixture.log().contains(".#checks.unit"));
    let manifest = fs::read(fixture.state.join("publish-roots.json")).unwrap();
    fixture.run("warm", &[("WRIX_CACHE_TEST_CHECKS", "1")]);
    assert!(fixture.present(&fixture.foreign).await);
    assert_eq!(
        fs::read(fixture.state.join("publish-roots.json")).unwrap(),
        manifest
    );
    assert!(!fixture.cache.exists());
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn warm_reads_repository_policy_and_checks_flag_overrides_environment() {
    if !native::in_isolated_store(
        "warm_reads_repository_policy_and_checks_flag_overrides_environment",
    ) {
        return;
    }
    for (policy, args) in [
        ("cache.warm.checks = true", vec![]),
        (
            "cache.warm.checks = false",
            vec![
                ("WRIX_CACHE_TEST_CHECKS", "1"),
                ("WRIX_CACHE_WARM_CHECKS", "0"),
            ],
        ),
    ] {
        let fixture = Fixture::new().await;
        fixture.set_roots(&[Fixture::root("checks.unit", &fixture.foreign)]);
        fixture.policy(policy);
        fixture.run("warm", &args);
        assert!(fixture.present(&fixture.foreign).await);
        assert!(fixture.log().contains("build --no-link .#checks.unit"));
    }
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn publish_environment_can_clear_repository_exclusions() {
    if !native::in_isolated_store("publish_environment_can_clear_repository_exclusions") {
        return;
    }
    let fixture = Fixture::new().await;
    fixture.policy("cache.publish.exclude_roots = ['.#packages.demo']");
    fixture.run("publish", &[]);
    assert!(!fixture.present(&fixture.output).await);
    fixture.run("publish", &[("WRIX_CACHE_PUBLISH_EXCLUDE", "")]);
    assert!(fixture.present(&fixture.output).await);
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn publication_does_not_prune_previous_entries_when_roots_change() {
    if !native::in_isolated_store("publication_does_not_prune_previous_entries_when_roots_change") {
        return;
    }
    let fixture = Fixture::new().await;
    fixture.run("publish", &[]);
    fixture.set_roots(&[]);
    fixture.run("publish", &[]);
    assert!(fixture.present(&fixture.output).await);
    let manifest: Value =
        serde_json::from_slice(&fs::read(fixture.state.join("publish-roots.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["roots"], json!([]));
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn lock_timeout_records_pending_and_explicit_publish_drains() {
    if !native::in_isolated_store("lock_timeout_records_pending_and_explicit_publish_drains") {
        return;
    }
    let fixture = Fixture::new().await;
    fs::copy(&fixture.roots, fixture.state.join("publish-roots.json")).unwrap();
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(fixture.state.join("cache.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    assert!(
        fixture
            .run("auto", &[("WRIX_CACHE_LOCK_TIMEOUT_MS", "0")])
            .contains("recorded pending publish")
    );
    assert!(
        fixture
            .failure("publish", &[("WRIX_CACHE_EXPLICIT_LOCK_TIMEOUT_MS", "0")])
            .contains("LockTimeout")
    );
    lock.unlock().unwrap();
    assert_eq!(
        fs::read_dir(fixture.state.join("pending")).unwrap().count(),
        1
    );
    fixture.run("publish", &[]);
    assert_eq!(
        fs::read_dir(fixture.state.join("pending")).unwrap().count(),
        0
    );
    assert!(fixture.present(&fixture.output).await);
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn automatic_publication_failure_preserves_candidates_for_explicit_retry() {
    if !native::in_isolated_store(
        "automatic_publication_failure_preserves_candidates_for_explicit_retry",
    ) {
        return;
    }
    let fixture = Fixture::new().await;
    Publisher::install(
        &fixture.state,
        fixture.workspace.hash(),
        &fixture.server.endpoint,
        CacheName::new(String::from("private")).unwrap(),
        &native::pull_token(),
        std::time::Duration::from_hours(720),
    )
    .unwrap();
    assert!(fixture.run("auto", &[]).contains("warning: automatic"));
    assert_eq!(
        fs::read_dir(fixture.state.join("pending")).unwrap().count(),
        1
    );
    assert!(!fixture.present(&fixture.output).await);
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn publish_reports_missing_authority_without_flat_fallback() {
    if !native::in_isolated_store("publish_reports_missing_authority_without_flat_fallback") {
        return;
    }
    let fixture = Fixture::new().await;
    fs::remove_file(fixture.state.join("celler-publisher.json")).unwrap();
    fixture.failure("publish", &[]);
    assert!(!fixture.cache.exists());
    assert!(!fixture.state.join("keys/cache.secret").exists());
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn unadmitted_hook_does_not_create_cache_control_or_archive_state() {
    if !native::in_isolated_store("unadmitted_hook_does_not_create_cache_control_or_archive_state")
    {
        return;
    }
    let fixture = Fixture::new().await;
    fs::remove_dir_all(&fixture.state).unwrap();
    assert!(
        fixture
            .run("auto", &[("WRIX_CACHE_TEST_DRV", "/nix/store/foreign.drv")])
            .contains("skipped non-project")
    );
    assert!(!fixture.state.exists());
    assert!(!fixture.cache.exists());
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn repository_pending_ttl_expires_records_without_direnv() {
    if !native::in_isolated_store("repository_pending_ttl_expires_records_without_direnv") {
        return;
    }
    let fixture = Fixture::new().await;
    fixture.policy("cache.pending_ttl = '1s'");
    let record = fixture.pending("expired", "/nix/store/packages.demo.drv", &fixture.output);
    fs::write(&record, json!({"created_at_epoch":1,"drv_path":"/nix/store/packages.demo.drv","out_paths":[fixture.output]}).to_string()).unwrap();
    assert!(fixture.run("status", &[]).contains("pending records: 0"));
    assert!(!record.exists());
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn status_reports_pending_work_and_previous_results_without_mutation() {
    if !native::in_isolated_store(
        "status_reports_pending_work_and_previous_results_without_mutation",
    ) {
        return;
    }
    let fixture = Fixture::new().await;
    fixture.run("publish", &[]);
    fixture.pending("pending", "/nix/store/packages.demo.drv", &fixture.output);
    let status = fs::read(fixture.state.join("cache-status.json")).unwrap();
    let report = fixture.run("status", &[]);
    assert!(report.contains("pending records: 1"));
    assert!(report.contains("last_publish: ok"));
    assert_eq!(
        fs::read(fixture.state.join("cache-status.json")).unwrap(),
        status
    );
    assert!(!fixture.cache.exists());
    assert!(!fixture.state.join("keys/cache.secret").exists());
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn status_reads_repository_soft_limit_without_direnv() {
    if !native::in_isolated_store("status_reads_repository_soft_limit_without_direnv") {
        return;
    }
    let fixture = Fixture::new().await;
    fixture.policy("cache.warn_size = 0");
    fs::create_dir(&fixture.cache).unwrap();
    fs::write(fixture.cache.join("unrelated-retained-data"), "data").unwrap();
    assert!(
        fixture
            .run("status", &[])
            .contains("warning: project cache size exceeds 0")
    );
    assert!(fixture.cache.join("unrelated-retained-data").exists());
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn daemon_publisher_uses_snapshot_without_ambient_config_or_repository_reads() {
    if !native::in_isolated_store(
        "daemon_publisher_uses_snapshot_without_ambient_config_or_repository_reads",
    ) {
        return;
    }
    let fixture = Fixture::new().await;
    fixture.run("publish", &[]);
    fixture.policy("cache.lock_timeout = 0");
    let settings = Settings::for_workspace(&fixture.workspace).unwrap();
    let snapshot = fixture.root.path().join("policy.json");
    fs::write(&snapshot, serde_json::to_vec(&settings).unwrap()).unwrap();
    fixture.policy("[wrix]\ninvalid = true");
    fs::create_dir(&fixture.cache).unwrap();
    let lock = OpenOptions::new()
        .write(true)
        .open(fixture.state.join("cache.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    let uid = Command::new("id").arg("-u").output().unwrap();
    let gid = Command::new("id").arg("-g").output().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_wrix-cache-hook"))
        .current_dir(fixture.workspace.canonical_path())
        .args(["--workspace-hash", fixture.workspace.hash().as_str()])
        .args(["--owner-uid", String::from_utf8(uid.stdout).unwrap().trim()])
        .args(["--owner-gid", String::from_utf8(gid.stdout).unwrap().trim()])
        .arg("--state-root")
        .arg(&fixture.state)
        .arg("--cache-root")
        .arg(&fixture.cache)
        .arg("--manifest")
        .arg(fixture.state.join("publish-roots.json"))
        .args([
            "--publisher-helper",
            env!("CARGO_BIN_EXE_wrix-cache-publish"),
        ])
        .arg("--config")
        .arg(snapshot)
        .env("DRV_PATH", "/nix/store/packages.demo.drv")
        .env("OUT_PATHS", &fixture.output)
        .env("WRIX_CACHE_LOCK_TIMEOUT_MS", "invalid")
        .output()
        .unwrap();
    lock.unlock().unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    assert!(String::from_utf8_lossy(&output.stdout).contains("recorded pending publish"));
    assert_eq!(
        fs::read_dir(fixture.state.join("pending")).unwrap().count(),
        1
    );
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn failed_warm_build_does_not_upload_or_drain_pending() {
    if !native::in_isolated_store("failed_warm_build_does_not_upload_or_drain_pending") {
        return;
    }
    let fixture = Fixture::new().await;
    let record = fixture.pending("matching", "/nix/store/packages.demo.drv", &fixture.output);
    fixture.failure("warm", &[("WRIX_TEST_BUILD_FAIL", "1")]);
    assert!(record.exists());
    assert!(!fixture.present(&fixture.output).await);
}

#[tokio::test]
#[ignore = "requires packaged Celler and isolated native Nix daemon"]
async fn native_store_queries_distinguish_realized_paths_and_invalid_input() {
    if !native::in_isolated_store(
        "native_store_queries_distinguish_realized_paths_and_invalid_input",
    ) {
        return;
    }
    let fixture = Fixture::new().await;
    assert!(
        wrix_cache::backend::paths_realized(&[fixture.output.display().to_string()])
            .await
            .unwrap()
    );
    assert!(!wrix_cache::backend::paths_realized(&[]).await.unwrap());
    assert!(
        wrix_cache::backend::paths_realized(&[String::from("not-a-store-path")])
            .await
            .is_err()
    );
}

#[test]
fn status_warns_above_soft_size_without_pruning() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let cache = root.path().join("cache");
    fs::create_dir(&cache).unwrap();
    let archive = std::fs::File::create(cache.join("retained-data")).unwrap();
    archive.set_len(50 * 1024 * 1024 * 1024 + 1).unwrap();
    let report = publisher::status_at(&state, &cache, Settings::default()).unwrap();
    assert!(
        report
            .lines()
            .iter()
            .any(|line| line.starts_with("warning: project cache size exceeds"))
    );
    assert_eq!(
        archive.metadata().unwrap().len(),
        50 * 1024 * 1024 * 1024 + 1
    );
    assert!(!state.join("keys/cache.secret").exists());
}

#[test]
fn status_does_not_report_legacy_workspace_pruning() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    fs::create_dir(&state).unwrap();
    let status = state.join("cache-status.json");
    let bytes = serde_json::to_vec(&json!({
        "dirty": true,
        "last_publish": "ok",
        "last_error": null,
        "last_prune": "ok",
        "last_prune_epoch": 1
    }))
    .unwrap();
    fs::write(&status, &bytes).unwrap();
    let report =
        publisher::status_at(&state, &root.path().join("cache"), Settings::default()).unwrap();
    assert!(report.lines().iter().any(|line| line == "last_publish: ok"));
    assert!(!report.lines().iter().any(|line| line.contains("prune")));
    assert_eq!(fs::read(status).unwrap(), bytes);
}

#[test]
fn upstream_fixture_models_partial_null_and_failed_queries() {
    let root = tempfile::tempdir().unwrap();
    let nix = root.path().join("nix");
    fs::write(&nix, NIX_FIXTURE).unwrap();
    fs::set_permissions(&nix, fs::Permissions::from_mode(0o755)).unwrap();
    for (mode, success, expected) in [
        ("normal", true, "{\"hit\":{},\"miss\":null}"),
        ("partial", false, "{\"hit\":{},\"miss\":null}"),
        ("malformed", true, "invalid JSON"),
        ("unavailable", false, ""),
    ] {
        let output = Command::new(&nix)
            .args([
                "path-info",
                "--store",
                "https://fixture.invalid",
                "--json",
                "hit",
                "miss",
            ])
            .env("WRIX_TEST_NIX_LOG", root.path().join("log"))
            .env("WRIX_TEST_UPSTREAM", mode)
            .env("WRIX_TEST_UPSTREAM_HIT", "hit")
            .output()
            .unwrap();
        assert_eq!(output.status.success(), success);
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
    }
    assert!(
        !Command::new(&nix)
            .arg("copy")
            .env("WRIX_TEST_NIX_LOG", root.path().join("log"))
            .status()
            .unwrap()
            .success()
    );
}

const NIX_FIXTURE: &str = r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$WRIX_TEST_NIX_LOG"
if [[ "$1" == build ]]; then
  exit "${WRIX_TEST_BUILD_FAIL:-0}"
fi
if [[ "$1" == path-info && "$2" == --store && "$4" == --json ]]; then
  case "$WRIX_TEST_UPSTREAM" in
    unavailable) exit 1 ;;
    malformed) printf 'invalid JSON\n'; exit 0 ;;
    normal|partial) ;;
    *) exit 2 ;;
  esac
  shift 4
  printf '{'
  separator=''
  for path in "$@"; do
    printf '%s"%s":' "$separator" "$path"
    if [[ "$path" == "$WRIX_TEST_UPSTREAM_HIT" ]]; then printf '{}'; else printf 'null'; fi
    separator=','
  done
  printf '}\n'
  [[ "$WRIX_TEST_UPSTREAM" == normal ]]
  exit "$?"
fi
printf 'unexpected Nix invocation\n' >&2
exit 2
"#;
