use std::{
    collections::{BTreeSet, HashSet},
    env,
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, SystemTime, SystemTimeError, UNIX_EPOCH},
};

use displaydoc::Display;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error as ThisError;
use wrix_core::{
    cache_key,
    path::{Workspace, WorkspaceHash, WorkspaceHashParseError},
};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// project cache I/O failed: {source}
    Io {
        #[from]
        source: io::Error,
    },
    /// invalid project cache JSON at {path}: {source}
    Json {
        path: String,
        source: serde_json::Error,
    },
    /// {source}
    WorkspaceHash {
        #[from]
        source: WorkspaceHashParseError,
    },
    /// system clock is before the Unix epoch: {source}
    Clock {
        #[from]
        source: SystemTimeError,
    },
    /// environment variable {name} must be valid Unicode
    InvalidUnicodeEnvironment { name: &'static str },
    /// environment variable {name} has invalid value {value}
    InvalidEnvironment { name: &'static str, value: String },
    /// command failed: {program}: {stderr}
    ProcessFailed { program: String, stderr: String },
    /// timed out waiting for project cache lock {path}
    LockTimeout { path: String },
    /// HOME is required to resolve wrix cache state roots
    HomeMissing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Publish,
    Warm { checks: bool },
    Prune,
    RotateKey,
}

#[derive(Clone, Debug)]
pub struct Report {
    lines: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Root {
    name: String,
    installable: String,
    #[serde(alias = "drvPath")]
    drv_path: String,
    #[serde(default, alias = "outPaths")]
    out_paths: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct RootManifest {
    #[serde(default)]
    schema_version: Option<u8>,
    #[serde(default)]
    workspace_hash: Option<String>,
    roots: Vec<Root>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PendingRecord {
    #[serde(default)]
    created_at_epoch: Option<u64>,
    #[serde(alias = "drvPath")]
    drv_path: String,
    #[serde(default, alias = "outPaths")]
    out_paths: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CacheStatus {
    dirty: bool,
    last_publish: Option<String>,
    last_prune: Option<String>,
    last_error: Option<String>,
    #[serde(default)]
    last_prune_epoch: Option<u64>,
}

#[derive(Clone, Debug)]
struct Paths {
    state_root: PathBuf,
    cache_root: PathBuf,
}

#[derive(Clone, Debug)]
struct Pending {
    path: PathBuf,
    drv_path: String,
    out_paths: Vec<String>,
}

impl Report {
    pub fn lines(&self) -> &[String] {
        &self.lines
    }
}

pub fn run_current_workspace(mode: Mode) -> Result<Report> {
    let workspace = Workspace::from_service_current_dir()?;
    let paths = Paths::for_workspace(workspace.hash())?;
    run_at(&workspace, &paths, mode)
}

pub fn run_workspace_at(
    workspace: &Workspace,
    state_root: &Path,
    cache_root: &Path,
    mode: Mode,
) -> Result<Report> {
    let paths = Paths {
        state_root: state_root.to_path_buf(),
        cache_root: cache_root.to_path_buf(),
    };
    run_at(workspace, &paths, mode)
}

pub fn status_current_workspace() -> Result<Report> {
    let workspace = Workspace::from_service_current_dir()?;
    let paths = Paths::for_workspace(workspace.hash())?;
    status_at_paths(&paths)
}

pub fn status_at(state_root: &Path, cache_root: &Path) -> Result<Report> {
    let paths = Paths {
        state_root: state_root.to_path_buf(),
        cache_root: cache_root.to_path_buf(),
    };
    status_at_paths(&paths)
}

fn run_at(workspace: &Workspace, paths: &Paths, mode: Mode) -> Result<Report> {
    paths.ensure()?;
    run(workspace, paths, mode)
}

fn status_at_paths(paths: &Paths) -> Result<Report> {
    paths.ensure()?;
    status_report(paths)
}

pub fn prune_stale_dirty(state_root: &Path, cache_root: &Path) -> Result<bool> {
    let paths = Paths {
        state_root: state_root.to_path_buf(),
        cache_root: cache_root.to_path_buf(),
    };
    if !cache_status_dirty(&paths)? || !prune_is_stale(&paths)? {
        return Ok(false);
    }
    paths.ensure()?;
    with_explicit_lock(&paths, || prune(&paths))?;
    Ok(true)
}

pub fn run_hook_record(
    workspace_hash: &str,
    state_root: &Path,
    cache_root: &Path,
    manifest: &Path,
    drv_path: &str,
    out_paths: &str,
) -> Result<Report> {
    validate_workspace_hash(workspace_hash)?;
    let paths = Paths {
        state_root: state_root.to_path_buf(),
        cache_root: cache_root.to_path_buf(),
    };
    paths.ensure()?;
    let roots = read_manifest_roots(manifest)?;
    let matching = roots
        .into_iter()
        .find(|root| root.drv_path == drv_path)
        .map(|root| Root {
            out_paths: split_paths(out_paths),
            ..root
        });
    let Some(root) = matching else {
        return Ok(Report {
            lines: vec![format!("skipped non-project derivation {drv_path}")],
        });
    };
    run_automatic_publish(&paths, root)
}

fn run(workspace: &Workspace, paths: &Paths, mode: Mode) -> Result<Report> {
    match mode {
        Mode::Publish => with_explicit_lock(paths, || {
            let roots = discover_roots(RootSet::Publish)?;
            write_manifest(&paths.publish_roots_path(), workspace.hash(), &roots)?;
            let realized = realized_roots(roots)?;
            let pending = read_pending(paths)?;
            publish_roots(paths, &realized.roots, pending, true).map(|mut report| {
                report.lines.extend(realized.unrealized);
                report
            })
        }),
        Mode::Warm { checks } => with_explicit_lock(paths, || {
            let root_set = RootSet::Warm { checks };
            let roots = discover_roots(root_set)?;
            build_roots(&roots)?;
            let realized = realized_roots(roots)?;
            write_manifest(
                &paths.publish_roots_path(),
                workspace.hash(),
                &realized.roots,
            )?;
            let pending = read_pending(paths)?;
            publish_roots(paths, &realized.roots, pending, true)
        }),
        Mode::Prune => with_explicit_lock(paths, || {
            prune(paths)?;
            Ok(Report {
                lines: vec![String::from("pruned project cache")],
            })
        }),
        Mode::RotateKey => with_explicit_lock(paths, || rotate_key(paths)),
    }
}

fn run_automatic_publish(paths: &Paths, root: Root) -> Result<Report> {
    let mut lines = Vec::new();
    let timeout = hook_lock_timeout()?;
    let Some(lock) = OperationLock::acquire(paths, Some(timeout), &mut lines)? else {
        record_pending(paths, &root.drv_path, &root.out_paths)?;
        lines.push(String::from(
            "warning: project cache lock timeout; recorded pending publish",
        ));
        return Ok(Report { lines });
    };
    let result = publish_roots(paths, &[root], Vec::new(), false);
    let release = lock.release();
    match (result, release) {
        (Ok(mut report), Ok(())) => {
            lines.append(&mut report.lines);
            Ok(Report { lines })
        }
        (Err(error), _) | (_, Err(error)) => {
            write_cache_status(paths, true, Some("warning"), None, Some(&error.to_string()))?;
            lines.push(format!(
                "warning: automatic project cache publish failed: {error}"
            ));
            Ok(Report { lines })
        }
    }
}

fn with_explicit_lock<T>(paths: &Paths, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let mut lines = Vec::new();
    let timeout = explicit_lock_timeout()?;
    let Some(lock) = OperationLock::acquire(paths, Some(timeout), &mut lines)? else {
        return Err(Error::LockTimeout {
            path: paths.cache_lock_path().display().to_string(),
        });
    };
    let result = operation();
    let release = lock.release();
    match (result, release) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

#[derive(Debug)]
struct OperationLock {
    file: File,
}

impl OperationLock {
    fn acquire(
        paths: &Paths,
        timeout: Option<Duration>,
        lines: &mut Vec<String>,
    ) -> Result<Option<Self>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(paths.cache_lock_path())?;
        let started = SystemTime::now();
        let mut announced = false;
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(Some(Self { file })),
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    if !announced {
                        lines.push(format!(
                            "waiting for project cache lock {}",
                            paths.cache_lock_path().display()
                        ));
                        announced = true;
                    }
                    if let Some(limit) = timeout
                        && started.elapsed()? >= limit
                    {
                        return Ok(None);
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn release(self) -> Result<()> {
        self.file.unlock()?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RootSet {
    Publish,
    Warm { checks: bool },
}

fn discover_roots(root_set: RootSet) -> Result<Vec<Root>> {
    let config = RootConfig::from_env(root_set)?;
    if let Some(path) = env::var_os("WRIX_CACHE_ROOTS_FILE") {
        let roots = roots_from_file(Path::new(&path))?;
        return filter_roots(roots, &config);
    }
    let system = current_system()?;
    let mut installables = Vec::new();
    if config.packages {
        installables.extend(attr_installables("packages", &system)?);
    }
    if config.checks {
        installables.extend(attr_installables("checks", &system)?);
    }
    if config.dev_shell
        && let Some(shell) = selected_dev_shell()?
    {
        let installable = format!(".#devShells.{system}.{shell}");
        if drv_path(&installable).is_ok() {
            installables.push((format!("devShells.{system}.{shell}"), installable));
        }
    }
    for (index, installable) in env_lines(config.include_env)?.into_iter().enumerate() {
        installables.push((format!("include.{index}"), installable));
    }
    let mut roots = Vec::new();
    for (name, installable) in installables {
        let drv_path = drv_path(&installable)?;
        roots.push(Root {
            name,
            installable,
            drv_path,
            out_paths: Vec::new(),
        });
    }
    filter_roots(roots, &config)
}

#[derive(Debug)]
struct RootConfig {
    packages: bool,
    checks: bool,
    dev_shell: bool,
    include_env: &'static str,
    exclude_env: &'static str,
}

impl RootConfig {
    fn from_env(root_set: RootSet) -> Result<Self> {
        match root_set {
            RootSet::Publish => Ok(Self {
                packages: env_bool("WRIX_CACHE_PUBLISH_PACKAGES", true)?,
                checks: env_bool("WRIX_CACHE_PUBLISH_CHECKS", true)?,
                dev_shell: env_bool("WRIX_CACHE_PUBLISH_DEVSHELL", true)?,
                include_env: "WRIX_CACHE_PUBLISH_INCLUDE",
                exclude_env: "WRIX_CACHE_PUBLISH_EXCLUDE",
            }),
            RootSet::Warm { checks } => Ok(Self {
                packages: env_bool("WRIX_CACHE_WARM_PACKAGES", true)?,
                checks: checks || env_bool("WRIX_CACHE_WARM_CHECKS", false)?,
                dev_shell: env_bool("WRIX_CACHE_WARM_DEVSHELL", true)?,
                include_env: "WRIX_CACHE_WARM_INCLUDE",
                exclude_env: "WRIX_CACHE_WARM_EXCLUDE",
            }),
        }
    }
}

fn filter_roots(roots: Vec<Root>, config: &RootConfig) -> Result<Vec<Root>> {
    let excludes: BTreeSet<String> = env_lines(config.exclude_env)?.into_iter().collect();
    Ok(roots
        .into_iter()
        .filter(|root| root_category_enabled(root, config))
        .filter(|root| !excludes.contains(&root.name) && !excludes.contains(&root.installable))
        .collect())
}

fn root_category_enabled(root: &Root, config: &RootConfig) -> bool {
    if root.name.starts_with("packages.") {
        return config.packages;
    }
    if root.name.starts_with("checks.") {
        return config.checks;
    }
    if root.name.starts_with("devShells.") {
        return config.dev_shell;
    }
    true
}

fn env_bool(name: &'static str, default: bool) -> Result<bool> {
    match env::var(name) {
        Ok(value) if matches!(value.as_str(), "0" | "false" | "no") => Ok(false),
        Ok(value) if matches!(value.as_str(), "1" | "true" | "yes") => Ok(true),
        Ok(value) => Err(Error::InvalidEnvironment { name, value }),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(env::VarError::NotUnicode(_)) => Err(Error::InvalidUnicodeEnvironment { name }),
    }
}

fn env_lines(name: &'static str) -> Result<Vec<String>> {
    match env::var(name) {
        Ok(value) => Ok(value
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()),
        Err(env::VarError::NotPresent) => Ok(Vec::new()),
        Err(env::VarError::NotUnicode(_)) => Err(Error::InvalidUnicodeEnvironment { name }),
    }
}

fn roots_from_file(path: &Path) -> Result<Vec<Root>> {
    let content = fs::read_to_string(path)?;
    let manifest =
        serde_json::from_str::<RootManifest>(&content).map_err(|source| Error::Json {
            path: path.display().to_string(),
            source,
        })?;
    let mut seen = HashSet::new();
    Ok(manifest
        .roots
        .into_iter()
        .filter(|root| seen.insert((root.name.clone(), root.drv_path.clone())))
        .collect())
}

fn attr_installables(kind: &str, system: &str) -> Result<Vec<(String, String)>> {
    let attr = format!(".#{}.{system}", kind);
    let output = Command::new(nix_bin()?)
        .arg("eval")
        .arg("--json")
        .arg(&attr)
        .arg("--apply")
        .arg("builtins.attrNames")
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(process_failed("nix eval", &output.stderr));
    }
    let names =
        serde_json::from_slice::<Vec<String>>(&output.stdout).map_err(|source| Error::Json {
            path: String::from("nix eval attrNames output"),
            source,
        })?;
    let mut installables = Vec::new();
    for name in names {
        installables.push((
            format!("{kind}.{system}.{name}"),
            format!(".#{kind}.{system}.{name}"),
        ));
    }
    Ok(installables)
}

fn selected_dev_shell() -> Result<Option<String>> {
    match env::var("WRIX_DEVSHELL") {
        Ok(value) if value.is_empty() || value == "none" => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(Some(String::from("default"))),
        Err(env::VarError::NotUnicode(_)) => Err(Error::InvalidUnicodeEnvironment {
            name: "WRIX_DEVSHELL",
        }),
    }
}

fn current_system() -> Result<String> {
    if let Ok(system) = env::var("WRIX_SYSTEM") {
        return Ok(system);
    }
    let output = Command::new(nix_bin()?)
        .arg("eval")
        .arg("--raw")
        .arg("--impure")
        .arg("--expr")
        .arg("builtins.currentSystem")
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    Err(process_failed(
        "nix eval builtins.currentSystem",
        &output.stderr,
    ))
}

fn drv_path(installable: &str) -> Result<String> {
    let output = Command::new(nix_bin()?)
        .arg("path-info")
        .arg("--derivation")
        .arg(installable)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    Err(process_failed("nix path-info --derivation", &output.stderr))
}

struct RealizedRoots {
    roots: Vec<Root>,
    unrealized: Vec<String>,
}

fn realized_roots(roots: Vec<Root>) -> Result<RealizedRoots> {
    let mut realized = Vec::new();
    let mut unrealized = Vec::new();
    for mut root in roots {
        if root.out_paths.is_empty() {
            if let Some(paths) = output_paths(&root.installable)? {
                root.out_paths = paths;
            } else {
                unrealized.push(format!("unrealized root: {}", root.installable));
                continue;
            }
        }
        if store_paths_valid(&root.out_paths)? {
            realized.push(root);
        } else {
            unrealized.push(format!("unrealized root: {}", root.installable));
        }
    }
    Ok(RealizedRoots {
        roots: realized,
        unrealized,
    })
}

fn store_paths_valid(paths: &[String]) -> Result<bool> {
    if paths.is_empty() {
        return Ok(false);
    }
    let output = Command::new(nix_store_bin()?)
        .arg("--check-validity")
        .arg("--print-invalid")
        .args(paths)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if !output.status.success() {
        return Err(process_failed("nix-store --check-validity", &output.stderr));
    }
    Ok(output.stdout.is_empty())
}

fn output_paths(installable: &str) -> Result<Option<Vec<String>>> {
    let output = Command::new(nix_bin()?)
        .arg("path-info")
        .arg("--json")
        .arg(installable)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let paths = parse_path_info_paths(&text)?;
    if paths.is_empty() {
        Ok(None)
    } else {
        Ok(Some(paths))
    }
}

fn parse_path_info_paths(input: &str) -> Result<Vec<String>> {
    let value: serde_json::Value = serde_json::from_str(input).map_err(|source| Error::Json {
        path: String::from("nix path-info output"),
        source,
    })?;
    let Some(object) = value.as_object() else {
        return Ok(Vec::new());
    };
    let mut paths = object
        .keys()
        .filter(|value| value.starts_with("/nix/store/"))
        .cloned()
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn build_roots(roots: &[Root]) -> Result<()> {
    if roots.is_empty() {
        return Ok(());
    }
    let mut command = Command::new(nix_bin()?);
    command.arg("build").arg("--no-link");
    for root in roots {
        command.arg(&root.installable);
    }
    let output = command
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(process_failed("nix build", &output.stderr))
    }
}

fn publish_roots(
    paths: &Paths,
    roots: &[Root],
    pending: Vec<Pending>,
    prune_after: bool,
) -> Result<Report> {
    let root_drvs = roots
        .iter()
        .map(|root| root.drv_path.as_str())
        .collect::<HashSet<_>>();
    let mut publishable = BTreeSet::new();
    let mut lines = Vec::new();
    for root in roots {
        let closure = closure_paths(&root.out_paths)?;
        update_gc_marker(paths, root, &closure)?;
        publishable.extend(closure);
        lines.push(format!("published root: {}", root.installable));
    }
    for record in pending {
        if root_drvs.contains(record.drv_path.as_str()) {
            for path in closure_paths(&record.out_paths)? {
                publishable.insert(path);
            }
            fs::remove_file(&record.path)?;
            lines.push(format!("drained pending: {}", record.path.display()));
        }
    }
    let filtered = subtract_upstream(paths, publishable)?;
    copy_to_cache(paths, &filtered)?;
    write_cache_status(paths, true, Some("ok"), None, None)?;
    if prune_after {
        prune(paths)?;
    }
    if filtered.is_empty() {
        lines.push(String::from("no project cache misses to copy"));
    } else {
        lines.push(format!("copied {} project cache paths", filtered.len()));
    }
    Ok(Report { lines })
}

fn closure_paths(out_paths: &[String]) -> Result<Vec<String>> {
    if out_paths.is_empty() {
        return Ok(Vec::new());
    }
    let output = Command::new(nix_store_bin()?)
        .arg("--query")
        .arg("--requisites")
        .args(out_paths)
        .stdin(Stdio::null())
        .output()?;
    if output.status.success() {
        return Ok(split_paths(&String::from_utf8_lossy(&output.stdout)));
    }
    Err(process_failed(
        "nix-store --query --requisites",
        &output.stderr,
    ))
}

fn subtract_upstream(paths: &Paths, candidates: BTreeSet<String>) -> Result<Vec<String>> {
    let substituters = upstream_substituters(paths)?;
    let mut filtered = Vec::new();
    for candidate in candidates {
        let mut upstream = false;
        for substituter in &substituters {
            if substitutable(substituter, &candidate)? {
                upstream = true;
                break;
            }
        }
        if upstream {
            continue;
        }
        filtered.push(candidate);
    }
    Ok(filtered)
}

fn upstream_substituters(paths: &Paths) -> Result<Vec<String>> {
    if let Ok(value) = env::var("WRIX_UPSTREAM_SUBSTITUTERS") {
        return Ok(value.split_whitespace().map(str::to_owned).collect());
    }
    let output = Command::new(nix_bin()?)
        .arg("config")
        .arg("show")
        .arg("substituters")
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(process_failed(
            "nix config show substituters",
            &output.stderr,
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let own = format!("file://{}", paths.cache_root.display());
    Ok(text
        .split_whitespace()
        .filter(|value| value.starts_with("http") || value.starts_with("file://"))
        .filter(|value| *value != own)
        .map(str::to_owned)
        .collect())
}

fn substitutable(substituter: &str, path: &str) -> Result<bool> {
    let status = Command::new(nix_bin()?)
        .arg("path-info")
        .arg("--store")
        .arg(substituter)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(status.success())
}

fn copy_to_cache(paths: &Paths, store_paths: &[String]) -> Result<()> {
    if store_paths.is_empty() {
        return Ok(());
    }
    let mut command = Command::new(nix_bin()?);
    command
        .arg("copy")
        .arg("--to")
        .arg(format!("file://{}", paths.cache_root.display()))
        .arg("--no-recursive")
        .arg("--secret-key-files")
        .arg(paths.cache_secret_path());
    for store_path in store_paths {
        command.arg(store_path);
    }
    let output = command
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(process_failed("nix copy", &output.stderr))
    }
}

fn prune(paths: &Paths) -> Result<()> {
    purge_expired_pending(paths)?;
    let reachable = marker_store_basenames(paths)?;
    if paths.cache_root.exists() {
        let mut stale_narinfos = Vec::new();
        let mut retained_payloads = BTreeSet::new();
        for entry in fs::read_dir(&paths.cache_root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("narinfo") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            let payloads = narinfo_payload_paths(paths, &path)?;
            if reachable.iter().any(|base| base.starts_with(stem)) {
                retained_payloads.extend(payloads);
            } else {
                stale_narinfos.push((path, payloads));
            }
        }
        for (narinfo, payloads) in stale_narinfos {
            fs::remove_file(narinfo)?;
            for payload in payloads {
                if !retained_payloads.contains(&payload) {
                    remove_cache_payload(&payload)?;
                }
            }
        }
        prune_unreferenced_payloads(paths, &retained_payloads)?;
        prune_unreachable_logs(paths, &reachable)?;
    }
    write_cache_status(paths, false, None, Some("ok"), None)
}

fn narinfo_payload_paths(paths: &Paths, narinfo: &Path) -> Result<Vec<PathBuf>> {
    let content = fs::read_to_string(narinfo)?;
    let mut payloads = Vec::new();
    for line in content.lines() {
        let Some(url) = line.strip_prefix("URL:") else {
            continue;
        };
        if let Some(path) = cache_payload_path(paths, url.trim()) {
            payloads.push(path);
        }
    }
    Ok(payloads)
}

fn cache_payload_path(paths: &Paths, url: &str) -> Option<PathBuf> {
    let relative = Path::new(url);
    if relative.is_absolute() || !relative.starts_with("nar") {
        return None;
    }
    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }
    Some(paths.cache_root.join(relative))
}

fn prune_unreferenced_payloads(paths: &Paths, retained: &BTreeSet<PathBuf>) -> Result<()> {
    let nar_dir = paths.cache_root.join("nar");
    if nar_dir.exists() {
        prune_payload_dir(&nar_dir, retained)?;
    }
    Ok(())
}

fn prune_unreachable_logs(paths: &Paths, reachable: &[String]) -> Result<()> {
    let log_dir = paths.cache_root.join("log");
    if !log_dir.exists() {
        return Ok(());
    }
    let retained_hashes = reachable
        .iter()
        .filter_map(|name| name.split_once('-').map(|(hash, _name)| hash))
        .collect::<Vec<_>>();
    prune_log_dir(&log_dir, &retained_hashes)
}

fn prune_log_dir(dir: &Path, retained_hashes: &[&str]) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            prune_log_dir(&path, retained_hashes)?;
            if directory_is_empty(&path)? {
                fs::remove_dir(path)?;
            }
        } else {
            let retained = path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| retained_hashes.iter().any(|hash| name.contains(hash)));
            if !retained {
                remove_cache_payload(&path)?;
            }
        }
    }
    Ok(())
}

fn prune_payload_dir(dir: &Path, retained: &BTreeSet<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            prune_payload_dir(&path, retained)?;
            if directory_is_empty(&path)? {
                fs::remove_dir(path)?;
            }
        } else if !retained.contains(&path) {
            remove_cache_payload(&path)?;
        }
    }
    Ok(())
}

fn directory_is_empty(path: &Path) -> Result<bool> {
    match fs::read_dir(path)?.next() {
        Some(Ok(_entry)) => Ok(false),
        Some(Err(error)) => Err(error.into()),
        None => Ok(true),
    }
}

fn remove_cache_payload(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn marker_store_basenames(paths: &Paths) -> Result<Vec<String>> {
    let mut basenames = Vec::new();
    if !paths.gcroots_dir().exists() {
        return Ok(basenames);
    }
    for entry in fs::read_dir(paths.gcroots_dir())? {
        let content = fs::read_to_string(entry?.path())?;
        for store_path in split_paths(&content) {
            if let Some(name) = Path::new(&store_path)
                .file_name()
                .and_then(|value| value.to_str())
            {
                basenames.push(name.to_owned());
            }
        }
    }
    Ok(basenames)
}

fn update_gc_marker(paths: &Paths, root: &Root, closure: &[String]) -> Result<()> {
    let marker = paths.gcroots_dir().join(safe_marker_name(&root.name));
    fs::write(marker, format!("{}\n", closure.join("\n")))?;
    Ok(())
}

fn read_pending(paths: &Paths) -> Result<Vec<Pending>> {
    let mut pending = Vec::new();
    if !paths.pending_dir().exists() {
        return Ok(pending);
    }
    for entry in fs::read_dir(paths.pending_dir())? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let record = read_pending_record(&path)?;
        if pending_expired(&path, &record)? {
            fs::remove_file(path)?;
            continue;
        }
        pending.push(Pending {
            path,
            drv_path: record.drv_path,
            out_paths: record.out_paths,
        });
    }
    Ok(pending)
}

fn read_pending_record(path: &Path) -> Result<PendingRecord> {
    let content = fs::read_to_string(path)?;
    serde_json::from_str(&content).map_err(|source| Error::Json {
        path: path.display().to_string(),
        source,
    })
}

fn record_pending(paths: &Paths, drv_path: &str, out_paths: &[String]) -> Result<PathBuf> {
    fs::create_dir_all(paths.pending_dir())?;
    let created_at_epoch = now_epoch()?;
    let filename = format!(
        "{}-{}-{}.json",
        created_at_epoch,
        std::process::id(),
        safe_pending_name(drv_path)
    );
    let path = paths.pending_dir().join(filename);
    let record = PendingRecord {
        created_at_epoch: Some(created_at_epoch),
        drv_path: drv_path.to_owned(),
        out_paths: out_paths.to_vec(),
    };
    write_json(&path, &record)?;
    Ok(path)
}

fn purge_expired_pending(paths: &Paths) -> Result<()> {
    if !paths.pending_dir().exists() {
        return Ok(());
    }
    for entry in fs::read_dir(paths.pending_dir())? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let record = read_pending_record(&path)?;
        if pending_expired(&path, &record)? {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn pending_expired(path: &Path, record: &PendingRecord) -> Result<bool> {
    let created_at = pending_created_at(path, record)?;
    Ok(now_epoch()?.saturating_sub(created_at) > pending_retention()?.as_secs())
}

fn pending_created_at(path: &Path, record: &PendingRecord) -> Result<u64> {
    if let Some(value) = record.created_at_epoch {
        return Ok(value);
    }
    let modified = fs::metadata(path)?.modified()?;
    system_time_epoch(modified)
}

fn read_manifest_roots(path: &Path) -> Result<Vec<Root>> {
    roots_from_file(path)
}

fn write_manifest(path: &Path, hash: &WorkspaceHash, roots: &[Root]) -> Result<()> {
    let manifest = RootManifest {
        schema_version: Some(1),
        workspace_hash: Some(hash.as_str().to_owned()),
        roots: roots.to_vec(),
    };
    write_json(path, &manifest)
}

fn status_report(paths: &Paths) -> Result<Report> {
    let pending = read_pending(paths)?;
    let mut ages = Vec::new();
    for record in &pending {
        ages.push(pending_file_age(&record.path)?);
    }
    let pending_age = ages
        .into_iter()
        .max()
        .map_or_else(|| String::from("none"), |age| format!("{}s", age.as_secs()));
    let cache_size = directory_size(&paths.cache_root)?;
    let status = read_cache_status(paths)?;
    let endpoints = read_endpoints_text(paths)?;
    let mut lines = vec![
        format!("cache size: {cache_size} bytes"),
        format!("pending records: {}", pending.len()),
        format!("oldest pending age: {pending_age}"),
        format!("dirty: {}", cache_status_dirty(paths)?),
        format!(
            "last_publish: {}",
            status.last_publish.as_deref().unwrap_or("null")
        ),
        format!(
            "last_prune: {}",
            status.last_prune.as_deref().unwrap_or("null")
        ),
        format!(
            "last_error: {}",
            status.last_error.as_deref().unwrap_or("null")
        ),
        format!("endpoints: {}", endpoints.trim()),
    ];
    let threshold = soft_size_threshold()?;
    if cache_size > threshold {
        lines.push(format!(
            "warning: project cache size exceeds {threshold} byte soft threshold"
        ));
    }
    Ok(Report { lines })
}

fn rotate_key(paths: &Paths) -> Result<Report> {
    wipe_cache(paths)?;
    generate_project_keypair(paths)?;
    write_cache_status(paths, false, None, Some("key-rotated"), None)?;
    Ok(Report {
        lines: vec![String::from(
            "rotated project cache key and invalidated local cache",
        )],
    })
}

fn wipe_cache(paths: &Paths) -> Result<()> {
    if paths.cache_root.exists() {
        fs::remove_dir_all(&paths.cache_root)?;
    }
    fs::create_dir_all(paths.cache_root.join("nar"))?;
    fs::create_dir_all(paths.cache_root.join("log"))?;
    fs::write(
        paths.cache_root.join("nix-cache-info"),
        "StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 40\n",
    )?;
    Ok(())
}

fn generate_project_keypair(paths: &Paths) -> Result<()> {
    cache_key::generate_keypair(
        &paths.key_name(),
        &paths.cache_secret_path(),
        &paths.cache_public_path(),
        &nix_store_bin()?,
    )?;
    Ok(())
}

impl Paths {
    fn for_workspace(hash: &WorkspaceHash) -> Result<Self> {
        let home = home_dir()?;
        let state_root = if cfg!(target_os = "macos") {
            home.join("Library/Application Support/wrix/workspaces")
                .join(hash.as_str())
        } else {
            env::var_os("XDG_STATE_HOME")
                .map_or_else(|| home.join(".local/state"), PathBuf::from)
                .join("wrix/workspaces")
                .join(hash.as_str())
        };
        let cache_root = if cfg!(target_os = "macos") {
            home.join("Library/Caches/wrix/workspaces")
                .join(hash.as_str())
                .join("binary-cache")
        } else {
            env::var_os("XDG_CACHE_HOME")
                .map_or_else(|| home.join(".cache"), PathBuf::from)
                .join("wrix/workspaces")
                .join(hash.as_str())
                .join("binary-cache")
        };
        Ok(Self {
            state_root,
            cache_root,
        })
    }

    fn ensure(&self) -> Result<()> {
        fs::create_dir_all(&self.state_root)?;
        fs::create_dir_all(&self.cache_root)?;
        fs::create_dir_all(self.gcroots_dir())?;
        fs::create_dir_all(self.pending_dir())?;
        fs::create_dir_all(self.keys_dir())?;
        fs::create_dir_all(self.cache_root.join("nar"))?;
        fs::create_dir_all(self.cache_root.join("log"))?;
        write_if_missing(&self.cache_lock_path(), "")?;
        write_json_if_missing(&self.cache_status_path(), &cache_status(false))?;
        cache_key::ensure_keypair(
            &self.key_name(),
            &self.cache_secret_path(),
            &self.cache_public_path(),
            &nix_store_bin()?,
        )?;
        write_if_missing(
            &self.cache_root.join("nix-cache-info"),
            "StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 40\n",
        )
    }

    fn cache_lock_path(&self) -> PathBuf {
        self.state_root.join("cache.lock")
    }

    fn cache_status_path(&self) -> PathBuf {
        self.state_root.join("cache-status.json")
    }

    fn gcroots_dir(&self) -> PathBuf {
        self.state_root.join("gcroots")
    }

    fn keys_dir(&self) -> PathBuf {
        self.state_root.join("keys")
    }

    fn pending_dir(&self) -> PathBuf {
        self.state_root.join("pending")
    }

    fn publish_roots_path(&self) -> PathBuf {
        self.state_root.join("publish-roots.json")
    }

    fn services_path(&self) -> PathBuf {
        self.state_root.join("services.json")
    }

    fn cache_secret_path(&self) -> PathBuf {
        self.keys_dir().join("cache.secret")
    }

    fn cache_public_path(&self) -> PathBuf {
        self.keys_dir().join("cache.pub")
    }

    fn key_name(&self) -> String {
        self.state_root
            .file_name()
            .and_then(|value| value.to_str())
            .map_or_else(
                || String::from("wrix-cache"),
                |value| format!("wrix-cache-{value}"),
            )
    }
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(Error::HomeMissing)
}

fn nix_bin() -> Result<String> {
    executable_from_env("WRIX_NIX_BIN", "nix")
}

fn nix_store_bin() -> Result<String> {
    executable_from_env("WRIX_NIX_STORE_BIN", "nix-store")
}

fn executable_from_env(name: &'static str, default: &str) -> Result<String> {
    match env::var(name) {
        Ok(value) if value.is_empty() => Err(Error::InvalidEnvironment { name, value }),
        Ok(value) => Ok(value),
        Err(env::VarError::NotPresent) => Ok(default.to_owned()),
        Err(env::VarError::NotUnicode(_)) => Err(Error::InvalidUnicodeEnvironment { name }),
    }
}

fn process_failed(program: &str, stderr: &[u8]) -> Error {
    Error::ProcessFailed {
        program: program.to_owned(),
        stderr: String::from_utf8_lossy(stderr).into_owned(),
    }
}

const fn cache_status(dirty: bool) -> CacheStatus {
    CacheStatus {
        dirty,
        last_publish: None,
        last_prune: None,
        last_error: None,
        last_prune_epoch: None,
    }
}

fn write_cache_status(
    paths: &Paths,
    dirty: bool,
    publish: Option<&str>,
    prune_status: Option<&str>,
    error: Option<&str>,
) -> Result<()> {
    let prior = read_cache_status(paths)?;
    let status = CacheStatus {
        dirty,
        last_publish: publish.map(str::to_owned).or(prior.last_publish),
        last_prune: prune_status.map(str::to_owned).or(prior.last_prune),
        last_error: error.map(str::to_owned).or_else(|| {
            if publish.is_some() || prune_status.is_some() {
                None
            } else {
                prior.last_error
            }
        }),
        last_prune_epoch: if prune_status.is_some() {
            Some(now_epoch()?)
        } else {
            prior.last_prune_epoch
        },
    };
    write_json(&paths.cache_status_path(), &status)
}

fn read_cache_status(paths: &Paths) -> Result<CacheStatus> {
    let path = paths.cache_status_path();
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(cache_status(false)),
        Err(error) => return Err(error.into()),
    };
    serde_json::from_str(&content).map_err(|source| Error::Json {
        path: path.display().to_string(),
        source,
    })
}

fn cache_status_dirty(paths: &Paths) -> Result<bool> {
    Ok(read_cache_status(paths)?.dirty)
}

fn prune_is_stale(paths: &Paths) -> Result<bool> {
    let Some(last_prune) = read_cache_status(paths)?.last_prune_epoch else {
        return Ok(true);
    };
    Ok(now_epoch()?.saturating_sub(last_prune) > prune_interval()?.as_secs())
}

fn write_if_missing(path: &Path, content: impl AsRef<[u8]>) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    fs::write(path, content)?;
    Ok(())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut content = serde_json::to_string_pretty(value).map_err(|source| Error::Json {
        path: path.display().to_string(),
        source,
    })?;
    content.push('\n');
    fs::write(path, content)?;
    Ok(())
}

fn write_json_if_missing(path: &Path, value: &impl Serialize) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    write_json(path, value)
}

fn split_paths(input: &str) -> Vec<String> {
    input
        .split_whitespace()
        .filter(|value| value.starts_with("/nix/store/"))
        .map(str::to_owned)
        .collect()
}

fn safe_marker_name(input: &str) -> String {
    input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn safe_pending_name(input: &str) -> String {
    Path::new(input)
        .file_name()
        .and_then(|value| value.to_str())
        .map_or_else(|| safe_marker_name(input), safe_marker_name)
}

fn now_epoch() -> Result<u64> {
    system_time_epoch(SystemTime::now())
}

fn system_time_epoch(time: SystemTime) -> Result<u64> {
    Ok(time.duration_since(UNIX_EPOCH)?.as_secs())
}

fn pending_file_age(path: &Path) -> Result<Duration> {
    Ok(fs::metadata(path)?.modified()?.elapsed()?)
}

fn hook_lock_timeout() -> Result<Duration> {
    Ok(Duration::from_millis(env_u64(
        "WRIX_CACHE_LOCK_TIMEOUT_MS",
        30_000,
    )?))
}

fn explicit_lock_timeout() -> Result<Duration> {
    let default = env_u64("WRIX_CACHE_LOCK_TIMEOUT_MS", 30_000)?;
    Ok(Duration::from_millis(env_u64(
        "WRIX_CACHE_EXPLICIT_LOCK_TIMEOUT_MS",
        default,
    )?))
}

fn pending_retention() -> Result<Duration> {
    const DEFAULT_PENDING_RETENTION_SECS: u64 = 604_800;
    env_seconds(
        "WRIX_CACHE_PENDING_RETENTION_SECS",
        Duration::from_secs(DEFAULT_PENDING_RETENTION_SECS),
    )
}

fn prune_interval() -> Result<Duration> {
    const DEFAULT_PRUNE_INTERVAL_SECS: u64 = 86_400;
    env_seconds(
        "WRIX_CACHE_PRUNE_INTERVAL_SECS",
        Duration::from_secs(DEFAULT_PRUNE_INTERVAL_SECS),
    )
}

fn soft_size_threshold() -> Result<u64> {
    env_u64("WRIX_CACHE_SOFT_LIMIT_BYTES", 50 * 1024 * 1024 * 1024)
}

fn env_seconds(name: &'static str, default: Duration) -> Result<Duration> {
    Ok(Duration::from_secs(env_u64(name, default.as_secs())?))
}

fn env_u64(name: &'static str, default: u64) -> Result<u64> {
    match env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .map_err(|_source| Error::InvalidEnvironment { name, value }),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(env::VarError::NotUnicode(_)) => Err(Error::InvalidUnicodeEnvironment { name }),
    }
}

fn directory_size(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            total += directory_size(&entry.path())?;
        } else {
            total += metadata.len();
        }
    }
    Ok(total)
}

fn read_endpoints_text(paths: &Paths) -> Result<String> {
    match fs::read_to_string(paths.services_path()) {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(String::from("unavailable")),
        Err(error) => Err(error.into()),
    }
}

fn validate_workspace_hash(hash: &str) -> Result<()> {
    WorkspaceHash::parse(hash)?;
    Ok(())
}

#[cfg(test)]
mod test {
    use super::{parse_path_info_paths, roots_from_file, safe_marker_name};
    use std::fs;

    #[test]
    fn parses_manifest_roots_with_out_paths() {
        let path =
            std::env::temp_dir().join(format!("wrix-cache-roots-{}.json", std::process::id()));
        fs::write(
            &path,
            r#"{"roots":[{"name":"pkg","installable":".#pkg","drv_path":"/nix/store/pkg.drv","out_paths":["/nix/store/pkg"]}]}"#,
        )
        .unwrap();
        let roots = roots_from_file(&path).unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].out_paths, vec![String::from("/nix/store/pkg")]);
    }

    #[test]
    fn extracts_output_paths_from_nix_path_info_json() {
        let paths = parse_path_info_paths(
            r#"{"/nix/store/aaa-root":{"deriver":"/nix/store/bbb-root.drv"}}"#,
        )
        .unwrap();
        assert_eq!(paths, vec![String::from("/nix/store/aaa-root")]);
    }

    #[test]
    fn marker_names_are_filesystem_safe() {
        assert_eq!(
            safe_marker_name("packages.x86_64-linux.demo"),
            "packages.x86_64-linux.demo"
        );
        assert_eq!(safe_marker_name("checks/a b"), "checks_a_b");
    }
}
