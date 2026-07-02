use std::{
    env, fs,
    fs::OpenOptions,
    io,
    path::{Path, PathBuf},
    process::Command,
};

use fs2::FileExt;
use wrix_cache::publisher::{self, Mode};
use wrix_core::path::Workspace;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const PROJECT_DRV: &str = "/nix/store/project-root.drv";
const PROJECT_OUT: &str = "/nix/store/project-root-out";
const PENDING_OUT: &str = "/nix/store/pending-root-out";
const UPSTREAM_OUT: &str = "/nix/store/upstream-dependency-out";
const VALID_PUBLIC_KEY: &str = "wrix-cache:CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC=\n";

#[test]
fn publish_realized_roots_drains_pending_and_updates_gc_markers() -> TestResult {
    let fixture = Fixture::new("publish-realized")?;
    fixture.write_roots(&[RootSpec::new(
        "packages.x86_64-linux.demo",
        ".#packages.x86_64-linux.demo",
        PROJECT_DRV,
        &[PROJECT_OUT],
    )])?;
    fixture.write_pending("matching", PROJECT_DRV, &[PENDING_OUT])?;

    let report = fixture.run("publish")?;

    assert!(report.contains("published root: .#packages.x86_64-linux.demo"));
    assert!(report.contains("drained pending:"));
    assert_eq!(fixture.pending_count()?, 0);
    let marker = fs::read_to_string(
        fixture
            .state_root
            .join("gcroots/packages.x86_64-linux.demo"),
    )?;
    assert!(marker.contains(PROJECT_OUT));
    let manifest = fs::read_to_string(fixture.state_root.join("publish-roots.json"))?;
    assert!(manifest.contains(fixture.workspace.hash().as_str()));
    let log = fixture.nix_log()?;
    assert!(log.contains(PROJECT_OUT));
    assert!(log.contains(PENDING_OUT));
    assert!(log.contains("--no-recursive"));

    Ok(())
}

#[test]
fn warm_roots_include_checks_only_when_requested() -> TestResult {
    let roots = [
        RootSpec::new(
            "packages.x86_64-linux.pkg",
            ".#packages.x86_64-linux.pkg",
            "/nix/store/pkg.drv",
            &["/nix/store/pkg-out"],
        ),
        RootSpec::new(
            "checks.x86_64-linux.unit",
            ".#checks.x86_64-linux.unit",
            "/nix/store/check.drv",
            &["/nix/store/check-out"],
        ),
        RootSpec::new(
            "devShells.x86_64-linux.default",
            ".#devShells.x86_64-linux.default",
            "/nix/store/shell.drv",
            &["/nix/store/shell-out"],
        ),
    ];

    let without_checks = Fixture::new("warm-without-checks")?;
    without_checks.write_roots(&roots)?;
    without_checks.run_warm(false)?;
    let log = without_checks.nix_log()?;
    assert!(log.contains(".#packages.x86_64-linux.pkg"));
    assert!(log.contains(".#devShells.x86_64-linux.default"));
    assert!(!log.contains(".#checks.x86_64-linux.unit"));

    let with_checks = Fixture::new("warm-with-checks")?;
    with_checks.write_roots(&roots)?;
    with_checks.run_warm(true)?;
    let log = with_checks.nix_log()?;
    assert!(log.contains(".#packages.x86_64-linux.pkg"));
    assert!(log.contains(".#devShells.x86_64-linux.default"));
    assert!(log.contains(".#checks.x86_64-linux.unit"));

    Ok(())
}

#[test]
fn publish_filters_to_current_workspace_closure() -> TestResult {
    let fixture = Fixture::new("publish-filters")?;
    fixture.write_roots(&[RootSpec::new(
        "packages.x86_64-linux.demo",
        ".#packages.x86_64-linux.demo",
        PROJECT_DRV,
        &[PROJECT_OUT],
    )])?;
    fixture.write_extra_closure(&[UPSTREAM_OUT])?;

    fixture.run("publish")?;

    let copy = fixture.copy_log_line()?;
    assert!(copy.contains(PROJECT_OUT));
    assert!(!copy.contains(UPSTREAM_OUT));
    assert!(!copy.contains("/nix/store/arbitrary-host-path"));

    Ok(())
}

#[test]
fn publish_uses_flat_signed_cache_with_nonrecursive_copies() -> TestResult {
    let fixture = Fixture::new("publish-copy-shape")?;
    fixture.write_roots(&[RootSpec::new(
        "packages.x86_64-linux.demo",
        ".#packages.x86_64-linux.demo",
        PROJECT_DRV,
        &[PROJECT_OUT],
    )])?;

    fixture.run("publish")?;

    let copy = fixture.copy_log_line()?;
    assert!(copy.contains(&format!("--to file://{}", fixture.cache_root.display())));
    assert!(copy.contains("--no-recursive"));
    assert!(copy.contains(&format!(
        "--secret-key-files {}",
        fixture.state_root.join("keys/cache.secret").display()
    )));
    assert!(copy.contains(PROJECT_OUT));

    Ok(())
}

#[test]
fn lock_timeout_records_pending_and_explicit_publish_drains() -> TestResult {
    let fixture = Fixture::new("lock-timeout")?;
    fixture.write_roots(&[RootSpec::new(
        "packages.x86_64-linux.demo",
        ".#packages.x86_64-linux.demo",
        PROJECT_DRV,
        &[PROJECT_OUT],
    )])?;
    fs::create_dir_all(&fixture.state_root)?;
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(fixture.state_root.join("cache.lock"))?;
    lock_file.try_lock_exclusive()?;

    let automatic = fixture.run_auto(
        PROJECT_DRV,
        PROJECT_OUT,
        &[(("WRIX_CACHE_LOCK_TIMEOUT_MS"), "100")],
    )?;
    assert!(automatic.contains("waiting for project cache lock"));
    assert!(automatic.contains("recorded pending publish"));
    assert_eq!(fixture.pending_count()?, 1);

    lock_file.unlock()?;
    drop(lock_file);

    let explicit = fixture.run("publish")?;
    assert!(explicit.contains("drained pending:"));
    assert_eq!(fixture.pending_count()?, 0);

    Ok(())
}

#[test]
fn prune_keeps_only_paths_reachable_from_current_markers() -> TestResult {
    let fixture = Fixture::new("prune-retention")?;
    fs::create_dir_all(fixture.state_root.join("gcroots"))?;
    fs::create_dir_all(fixture.cache_root.join("nar"))?;
    fs::write(
        fixture.state_root.join("gcroots/packages.demo"),
        "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-live\n",
    )?;
    fs::write(
        fixture
            .cache_root
            .join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.narinfo"),
        "StorePath: /nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-live\nURL: nar/live.nar\n",
    )?;
    fs::write(fixture.cache_root.join("nar/live.nar"), "live\n")?;
    fs::write(
        fixture
            .cache_root
            .join("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.narinfo"),
        "StorePath: /nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-stale\nURL: nar/stale.nar\n",
    )?;
    fs::write(fixture.cache_root.join("nar/stale.nar"), "stale\n")?;
    fs::write(fixture.cache_root.join("nar/orphan.nar"), "orphan\n")?;

    fixture.run("prune")?;

    assert!(
        fixture
            .cache_root
            .join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.narinfo")
            .exists()
    );
    assert!(fixture.cache_root.join("nar/live.nar").exists());
    assert!(
        !fixture
            .cache_root
            .join("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.narinfo")
            .exists()
    );
    assert!(!fixture.cache_root.join("nar/stale.nar").exists());
    assert!(!fixture.cache_root.join("nar/orphan.nar").exists());
    let status = fs::read_to_string(fixture.state_root.join("cache-status.json"))?;
    assert!(status.contains("\"dirty\": false"));

    Ok(())
}

#[test]
fn rotate_key_invalidates_cache_and_replaces_trust_root() -> TestResult {
    let fixture = Fixture::new("rotate-key")?;
    fs::create_dir_all(fixture.state_root.join("keys"))?;
    fs::create_dir_all(fixture.cache_root.join("nar"))?;
    fs::write(fixture.state_root.join("keys/cache.secret"), "old-secret\n")?;
    fs::write(fixture.state_root.join("keys/cache.pub"), VALID_PUBLIC_KEY)?;
    fs::write(fixture.cache_root.join("old.narinfo"), "old\n")?;
    fs::write(fixture.cache_root.join("nar/old.nar"), "old\n")?;

    let old_public = fs::read_to_string(fixture.state_root.join("keys/cache.pub"))?;
    let report = fixture.run("rotate")?;
    let new_public = fs::read_to_string(fixture.state_root.join("keys/cache.pub"))?;

    assert!(report.contains("rotated project cache key"));
    assert_ne!(new_public, old_public);
    assert!(!fixture.cache_root.join("old.narinfo").exists());
    assert!(!fixture.cache_root.join("nar/old.nar").exists());
    assert!(fixture.cache_root.join("nix-cache-info").is_file());
    assert!(fixture.cache_root.join("nar").is_dir());
    let status = fs::read_to_string(fixture.state_root.join("cache-status.json"))?;
    assert!(status.contains("key-rotated"));

    Ok(())
}

#[test]
fn status_warns_above_soft_size_without_pruning() -> TestResult {
    let fixture = Fixture::new("status-soft-limit")?;
    fs::create_dir_all(fixture.cache_root.join("nar"))?;
    let retained = fixture.cache_root.join("nar/retained.nar");
    fs::write(&retained, "retained cache payload\n")?;

    let report = fixture.run_with_env("status", &[("WRIX_CACHE_SOFT_LIMIT_BYTES", "1")])?;

    assert!(report.contains("cache size:"));
    assert!(report.contains("warning: project cache size exceeds 1 byte soft threshold"));
    assert!(retained.exists());

    Ok(())
}

#[test]
#[ignore = "child process helper receives faked command environment"]
fn publisher_child() -> TestResult {
    let action = env::var("WRIX_CACHE_TEST_ACTION")?;
    let workspace_path = PathBuf::from(env::var("WRIX_CACHE_TEST_WORKSPACE")?);
    let state_root = PathBuf::from(env::var("WRIX_CACHE_TEST_STATE_ROOT")?);
    let cache_root = PathBuf::from(env::var("WRIX_CACHE_TEST_CACHE_ROOT")?);
    let output_path = PathBuf::from(env::var("WRIX_CACHE_TEST_OUTPUT")?);
    let workspace = Workspace::from_path(workspace_path)?;
    let report = match action.as_str() {
        "publish" => {
            publisher::run_workspace_at(&workspace, &state_root, &cache_root, Mode::Publish)?
        }
        "warm" => publisher::run_workspace_at(
            &workspace,
            &state_root,
            &cache_root,
            Mode::Warm {
                checks: env::var("WRIX_CACHE_TEST_CHECKS").is_ok_and(|value| value == "1"),
            },
        )?,
        "prune" => publisher::run_workspace_at(&workspace, &state_root, &cache_root, Mode::Prune)?,
        "rotate" => {
            publisher::run_workspace_at(&workspace, &state_root, &cache_root, Mode::RotateKey)?
        }
        "status" => publisher::status_at(&state_root, &cache_root)?,
        "auto" => publisher::run_hook_record(
            workspace.hash().as_str(),
            &state_root,
            &cache_root,
            Path::new(&env::var("WRIX_CACHE_TEST_ROOTS_FILE")?),
            &env::var("WRIX_CACHE_TEST_DRV_PATH")?,
            &env::var("WRIX_CACHE_TEST_OUT_PATHS")?,
        )?,
        other => return Err(io::Error::other(format!("unknown action {other}")).into()),
    };
    fs::write(output_path, format!("{}\n", report.lines().join("\n")))?;
    Ok(())
}

struct RootSpec<'a> {
    name: &'a str,
    installable: &'a str,
    drv_path: &'a str,
    out_paths: Vec<&'a str>,
}

impl<'a> RootSpec<'a> {
    fn new(name: &'a str, installable: &'a str, drv_path: &'a str, out_paths: &[&'a str]) -> Self {
        Self {
            name,
            installable,
            drv_path,
            out_paths: out_paths.to_vec(),
        }
    }
}

struct Fixture {
    root: tempfile::TempDir,
    workspace: Workspace,
    state_root: PathBuf,
    cache_root: PathBuf,
    roots_file: PathBuf,
    extra_closure_file: PathBuf,
    nix_bin: PathBuf,
    nix_store_bin: PathBuf,
    nix_log: PathBuf,
    nix_store_log: PathBuf,
    key_counter: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> TestResult<Self> {
        let root = tempfile::Builder::new().prefix(name).tempdir()?;
        let workspace_path = root.path().join("workspace");
        fs::create_dir_all(&workspace_path)?;
        let workspace = Workspace::from_path(&workspace_path)?;
        let state_root = root.path().join("state");
        let cache_root = root.path().join("cache");
        let roots_file = root.path().join("roots.json");
        let extra_closure_file = root.path().join("extra-closure");
        let bin_dir = root.path().join("bin");
        fs::create_dir_all(&bin_dir)?;
        let nix_bin = bin_dir.join("nix");
        let nix_store_bin = bin_dir.join("nix-store");
        let nix_log = root.path().join("nix.log");
        let nix_store_log = root.path().join("nix-store.log");
        let key_counter = root.path().join("key-counter");
        write_fake_nix(&nix_bin)?;
        write_fake_nix_store(&nix_store_bin)?;
        Ok(Self {
            root,
            workspace,
            state_root,
            cache_root,
            roots_file,
            extra_closure_file,
            nix_bin,
            nix_store_bin,
            nix_log,
            nix_store_log,
            key_counter,
        })
    }

    fn write_roots(&self, roots: &[RootSpec<'_>]) -> io::Result<()> {
        let mut content = String::from("{\n  \"roots\": [\n");
        for (index, root) in roots.iter().enumerate() {
            let comma = if index + 1 == roots.len() { "" } else { "," };
            let out_paths = root
                .out_paths
                .iter()
                .map(|path| format!("\"{path}\""))
                .collect::<Vec<_>>()
                .join(", ");
            content.push_str("    { \"name\": \"");
            content.push_str(root.name);
            content.push_str("\", \"installable\": \"");
            content.push_str(root.installable);
            content.push_str("\", \"drv_path\": \"");
            content.push_str(root.drv_path);
            content.push_str("\", \"out_paths\": [");
            content.push_str(&out_paths);
            content.push_str("] }");
            content.push_str(comma);
            content.push('\n');
        }
        content.push_str("  ]\n}\n");
        fs::write(&self.roots_file, content)
    }

    fn write_pending(&self, name: &str, drv_path: &str, out_paths: &[&str]) -> io::Result<()> {
        let pending_dir = self.state_root.join("pending");
        fs::create_dir_all(&pending_dir)?;
        fs::write(
            pending_dir.join(format!("{name}.json")),
            format!(
                "{{\n  \"drv_path\": \"{drv_path}\",\n  \"out_paths\": [{}]\n}}\n",
                out_paths
                    .iter()
                    .map(|path| format!("\"{path}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
    }

    fn write_extra_closure(&self, paths: &[&str]) -> io::Result<()> {
        fs::write(&self.extra_closure_file, format!("{}\n", paths.join("\n")))
    }

    fn run(&self, action: &str) -> TestResult<String> {
        self.run_with_env(action, &[])
    }

    fn run_warm(&self, checks: bool) -> TestResult<String> {
        if checks {
            self.run_with_env("warm", &[("WRIX_CACHE_TEST_CHECKS", "1")])
        } else {
            self.run_with_env("warm", &[])
        }
    }

    fn run_auto(
        &self,
        drv_path: &str,
        out_paths: &str,
        extra_env: &[(&str, &str)],
    ) -> TestResult<String> {
        let output = self.root.path().join("auto.out");
        let mut command = self.child_command("auto", &output)?;
        command
            .env("WRIX_CACHE_TEST_DRV_PATH", drv_path)
            .env("WRIX_CACHE_TEST_OUT_PATHS", out_paths)
            .env("WRIX_CACHE_TEST_ROOTS_FILE", &self.roots_file);
        for (key, value) in extra_env {
            command.env(key, value);
        }
        Self::finish_child(command, &output)
    }

    fn run_with_env(&self, action: &str, extra_env: &[(&str, &str)]) -> TestResult<String> {
        let output = self.root.path().join(format!("{action}.out"));
        let mut command = self.child_command(action, &output)?;
        for (key, value) in extra_env {
            command.env(key, value);
        }
        Self::finish_child(command, &output)
    }

    fn child_command(&self, action: &str, output: &Path) -> TestResult<Command> {
        let mut command = Command::new(env::current_exe()?);
        command
            .arg("publisher_child")
            .arg("--exact")
            .arg("--ignored")
            .env("WRIX_CACHE_TEST_ACTION", action)
            .env("WRIX_CACHE_TEST_WORKSPACE", self.workspace.canonical_path())
            .env("WRIX_CACHE_TEST_STATE_ROOT", &self.state_root)
            .env("WRIX_CACHE_TEST_CACHE_ROOT", &self.cache_root)
            .env("WRIX_CACHE_TEST_OUTPUT", output)
            .env("WRIX_NIX_BIN", &self.nix_bin)
            .env("WRIX_NIX_STORE_BIN", &self.nix_store_bin)
            .env("WRIX_FAKE_NIX_LOG", &self.nix_log)
            .env("WRIX_FAKE_NIX_STORE_LOG", &self.nix_store_log)
            .env("WRIX_FAKE_KEY_COUNTER", &self.key_counter)
            .env("WRIX_FAKE_EXTRA_CLOSURE_FILE", &self.extra_closure_file)
            .env("WRIX_FAKE_UPSTREAM_MATCH", "upstream")
            .env("WRIX_UPSTREAM_SUBSTITUTERS", "https://cache.example");
        if self.roots_file.exists() {
            command.env("WRIX_CACHE_ROOTS_FILE", &self.roots_file);
        }
        Ok(command)
    }

    fn finish_child(mut command: Command, output: &Path) -> TestResult<String> {
        let child = command.output()?;
        if !child.status.success() {
            return Err(io::Error::other(format!(
                "publisher child failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr)
            ))
            .into());
        }
        Ok(fs::read_to_string(output)?)
    }

    fn pending_count(&self) -> io::Result<usize> {
        let pending_dir = self.state_root.join("pending");
        if !pending_dir.exists() {
            return Ok(0);
        }
        Ok(fs::read_dir(pending_dir)?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.path().extension().and_then(|value| value.to_str()) == Some("json")
            })
            .count())
    }

    fn nix_log(&self) -> io::Result<String> {
        fs::read_to_string(&self.nix_log)
    }

    fn copy_log_line(&self) -> TestResult<String> {
        self.nix_log()?
            .lines()
            .find(|line| line.starts_with("nix copy "))
            .map(str::to_owned)
            .ok_or_else(|| io::Error::other("missing nix copy log line").into())
    }
}

fn write_fake_nix(path: &Path) -> io::Result<()> {
    fs::write(
        path,
        r#"#!/usr/bin/env bash
set -euo pipefail

log="$WRIX_FAKE_NIX_LOG"
printf 'nix' >>"$log"
for arg in "$@"; do
  printf ' %s' "$arg" >>"$log"
done
printf '\n' >>"$log"

if [[ "$#" -ge 4 && "$1" == "path-info" && "$2" == "--store" ]]; then
  candidate="$4"
  if [[ -n "$WRIX_FAKE_UPSTREAM_MATCH" && "$candidate" == *"$WRIX_FAKE_UPSTREAM_MATCH"* ]]; then
    exit 0
  fi
  exit 1
fi

case "$1" in
  build|copy)
    exit 0
    ;;
  config)
    printf 'substituters = https://cache.example\n'
    ;;
  path-info)
    printf '{}\n'
    ;;
  *)
    printf 'unsupported fake nix command: %s\n' "$*" >&2
    exit 2
    ;;
esac
"#,
    )?;
    make_executable(path)
}

fn write_fake_nix_store(path: &Path) -> io::Result<()> {
    fs::write(
        path,
        r#"#!/usr/bin/env bash
set -euo pipefail

log="$WRIX_FAKE_NIX_STORE_LOG"
printf 'nix-store' >>"$log"
for arg in "$@"; do
  printf ' %s' "$arg" >>"$log"
done
printf '\n' >>"$log"

case "$1" in
  --generate-binary-cache-key)
    key_name="$2"
    secret_path="$3"
    public_path="$4"
    counter_file="$WRIX_FAKE_KEY_COUNTER"
    count=0
    if [[ -f "$counter_file" ]]; then
      count="$(<"$counter_file")"
    fi
    count=$((count + 1))
    printf '%s\n' "$count" >"$counter_file"
    encoded='AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA='
    if [[ "$count" != "1" ]]; then
      encoded='BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB='
    fi
    printf '%s-secret-%s\n' "$key_name" "$count" >"$secret_path"
    printf '%s:%s\n' "$key_name" "$encoded" >"$public_path"
    ;;
  --check-validity)
    exit 0
    ;;
  --query)
    shift 2
    for store_path in "$@"; do
      printf '%s\n' "$store_path"
    done
    if [[ -f "$WRIX_FAKE_EXTRA_CLOSURE_FILE" ]]; then
      cat "$WRIX_FAKE_EXTRA_CLOSURE_FILE"
    fi
    ;;
  *)
    printf 'unsupported fake nix-store command: %s\n' "$*" >&2
    exit 2
    ;;
esac
"#,
    )?;
    make_executable(path)
}

#[cfg(unix)]
fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}
