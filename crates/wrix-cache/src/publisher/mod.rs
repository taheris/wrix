use std::{
    collections::{BTreeSet, HashSet},
    env,
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, ErrorKind},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, SystemTime, SystemTimeError, UNIX_EPOCH},
};

use displaydoc::Display;
use fs2::FileExt;
use serde::Serialize;
use thiserror::Error as ThisError;
use wrix_core::{
    cache_key,
    path::{Workspace, WorkspaceHash, WorkspaceHashParseError},
};

use crate::config::{Roots, Settings};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// workspace admission failed: {0}
    Admission(#[from] wrix_state::store::Error),
    /// project cache I/O failed: {source}
    Io { source: io::Error },
    /// invalid project cache JSON at {path}: {source}
    Json {
        path: String,
        source: serde_json::Error,
    },
    /// {source}
    WorkspaceHash { source: WorkspaceHashParseError },
    /// system clock is before the Unix epoch: {source}
    Clock { source: SystemTimeError },
    /// environment variable {name} must be valid Unicode
    InvalidUnicodeEnvironment { name: &'static str },
    /// environment variable {name} has invalid value {value}
    InvalidEnvironment { name: &'static str, value: String },
    /// command failed: {program}: {stderr}
    ProcessFailed { program: String, stderr: String },
    /// timed out waiting for project cache lock {path}
    LockTimeout { path: String },
    /// {0}
    Config(#[from] crate::config::ReadError),
    /// HOME is required to resolve wrix cache state roots
    HomeMissing,
    /// {0}
    Publication(#[from] crate::backend::publication::Error),
    /// {0}
    Backend(#[from] crate::backend::Error),
    /// Celler view was rotated, but local key/status recording failed; no rollback occurred: {source}
    RotationRecording { source: Box<Error> },
}

impl From<io::Error> for Error {
    fn from(source: io::Error) -> Self {
        Self::Io { source }
    }
}

impl From<WorkspaceHashParseError> for Error {
    fn from(source: WorkspaceHashParseError) -> Self {
        Self::WorkspaceHash { source }
    }
}

impl From<SystemTimeError> for Error {
    fn from(source: SystemTimeError) -> Self {
        Self::Clock { source }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Publish,
    Warm { checks: bool },
    RotateKey,
}

#[derive(Clone, Debug)]
pub struct Report {
    lines: Vec<String>,
}

use wrix_state::namespace::document::cache::{
    Manifest as RootManifest, Pending as PendingRecord, Root, Status as CacheStatus,
};

#[derive(Clone, Debug)]
struct Paths {
    scope: Option<wrix_state::context::Context>,
    state_root: PathBuf,
    cache_root: PathBuf,
    settings: Settings,
}

#[derive(Clone, Debug)]
struct Pending {
    path: PathBuf,
    created_at_epoch: u64,
    drv_path: String,
    out_paths: Vec<String>,
}

impl Report {
    pub fn lines(&self) -> &[String] {
        &self.lines
    }
}

pub fn run_current_workspace(mode: Mode) -> Result<Report> {
    let selection = wrix_state::context::Selection::inspect(&env::current_dir()?)?;
    let workspace = selection.workspace();
    let settings = Settings::for_workspace(workspace)?;
    selection.authorize_effects()?;
    let mut paths = Paths::for_workspace(workspace.hash(), settings)?;
    paths.scope = selection.context().cloned();
    run_at(workspace, &paths, mode)
}

pub fn run_workspace_at(
    workspace: &Workspace,
    state_root: &Path,
    cache_root: &Path,
    mode: Mode,
) -> Result<Report> {
    let paths = Paths {
        scope: scope_for_workspace(workspace, state_root)?,
        state_root: state_root.to_path_buf(),
        cache_root: cache_root.to_path_buf(),
        settings: Settings::for_workspace(workspace)?,
    };
    run_at(workspace, &paths, mode)
}

fn scope_for_workspace(
    workspace: &Workspace,
    state_root: &Path,
) -> Result<Option<wrix_state::context::Context>> {
    if !wrix_state::namespace::is_host_root(state_root)? {
        return Ok(None);
    }
    let roots = wrix_core::host_path::Roots::discover()?;
    if roots.workspace_state(workspace.hash()).as_os_str() != state_root.as_os_str() {
        return Err(wrix_state::store::Error::Conflict.into());
    }
    let selection = wrix_state::context::Selection::inspect(workspace.canonical_path())?;
    if selection.workspace().hash() != workspace.hash()
        || selection.workspace().canonical_path().as_os_str()
            != workspace.canonical_path().as_os_str()
    {
        return Err(wrix_state::store::Error::Conflict.into());
    }
    selection.authorize_effects()?;
    Ok(selection.context().cloned())
}

pub fn status_current_workspace() -> Result<Report> {
    let workspace = wrix_state::context::service_workspace(&env::current_dir()?, false)?;
    let paths = Paths::for_workspace(workspace.hash(), Settings::for_workspace(&workspace)?)?;
    status_at_paths(&paths)
}

pub fn status_at(state_root: &Path, cache_root: &Path, settings: Settings) -> Result<Report> {
    let paths = Paths {
        scope: None,
        state_root: state_root.to_path_buf(),
        cache_root: cache_root.to_path_buf(),
        settings,
    };
    status_at_paths(&paths)
}

fn run_at(workspace: &Workspace, paths: &Paths, mode: Mode) -> Result<Report> {
    match mode {
        Mode::Publish | Mode::Warm { .. } => paths.ensure_control()?,
        Mode::RotateKey => (),
    }
    run(workspace, paths, mode)
}

fn status_at_paths(paths: &Paths) -> Result<Report> {
    paths.ensure_control()?;
    status_report(paths)
}

#[derive(Clone, Copy, Debug)]
pub struct HookRecord<'a> {
    pub workspace_hash: &'a WorkspaceHash,
    pub state_root: &'a Path,
    pub cache_root: &'a Path,
    pub manifest: &'a Path,
    pub drv_path: &'a str,
    pub out_paths: &'a str,
}

pub fn run_hook_record(record: HookRecord<'_>, settings: Settings) -> Result<Report> {
    let HookRecord {
        workspace_hash,
        state_root,
        cache_root,
        manifest,
        drv_path,
        out_paths,
    } = record;
    tracing::debug!(%workspace_hash, "processing automatic cache publish");
    let paths = Paths {
        scope: wrix_state::namespace::context_at(state_root)?,
        state_root: state_root.to_path_buf(),
        cache_root: cache_root.to_path_buf(),
        settings,
    };
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
    paths.ensure_control()?;
    run_automatic_publish(&paths, workspace_hash, &root)
}

fn run(workspace: &Workspace, paths: &Paths, mode: Mode) -> Result<Report> {
    let cwd = workspace.canonical_path();
    match mode {
        Mode::Publish => with_explicit_lock(paths, || {
            let roots = discover_roots(RootSet::Publish, &paths.settings, cwd)?;
            paths.authorize()?;
            write_manifest(&paths.publish_roots_path(), workspace.hash(), &roots)?;
            let realized = realized_roots(roots)?;
            let pending = read_pending(paths)?;
            publish_roots(paths, workspace.hash(), &realized.roots, pending).map(|mut report| {
                report.lines.extend(realized.unrealized);
                report
            })
        }),
        Mode::Warm { checks } => with_explicit_lock(paths, || {
            let configured = discover_roots(RootSet::Publish, &paths.settings, cwd)?;
            let warm = discover_roots(RootSet::Warm { checks }, &paths.settings, cwd)?;
            paths.authorize()?;
            write_manifest(&paths.publish_roots_path(), workspace.hash(), &configured)?;
            build_roots(&warm, cwd)?;
            let selected = warm
                .iter()
                .map(|root| root.installable.as_str())
                .collect::<HashSet<_>>();
            let roots = configured
                .iter()
                .filter(|root| selected.contains(root.installable.as_str()))
                .cloned()
                .collect();
            let realized = realized_roots(roots, cwd)?;
            let pending = read_pending(paths)?;
            publish_roots(paths, workspace.hash(), &realized.roots, pending)
        }),
        Mode::RotateKey => with_explicit_lock(paths, || rotate_key(paths, workspace.hash())),
    }
}

fn run_automatic_publish(paths: &Paths, workspace: &WorkspaceHash, root: &Root) -> Result<Report> {
    let mut lines = Vec::new();
    let timeout = Duration::from_millis(paths.settings.lock_timeout_ms);
    let Some(lock) = OperationLock::acquire(paths, Some(timeout), &mut lines)? else {
        record_pending(paths, &root.drv_path, &root.out_paths)?;
        lines.push(String::from(
            "warning: project cache lock timeout; recorded pending publish",
        ));
        return Ok(Report { lines });
    };
    let result = publish_hook_root(paths, workspace, root);
    let release = lock.release();
    match (result, release) {
        (Ok(mut report), Ok(())) => lines.append(&mut report.lines),
        (Err(error), _) | (_, Err(error)) => {
            record_pending(paths, &root.drv_path, &root.out_paths)?;
            write_cache_status(paths, true, Some("warning"), Some(&error.to_string()))?;
            lines.push(format!(
                "warning: automatic project cache publish failed: {error}"
            ));
        }
    }
    Ok(Report { lines })
}

fn publish_hook_root(paths: &Paths, workspace: &WorkspaceHash, root: &Root) -> Result<Report> {
    let closure = closure_paths(&root.out_paths)?;
    let filtered = subtract_upstream(paths, closure.into_iter().collect())?;
    let count = publish_selected(paths, workspace, &filtered)?;
    write_cache_status(paths, true, Some("ok"), None)?;
    Ok(Report {
        lines: vec![format!(
            "published {count} project cache paths through Celler"
        )],
    })
}

fn with_explicit_lock<T>(paths: &Paths, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    paths.authorize()?;
    let mut lines = Vec::new();
    let timeout = Duration::from_millis(paths.settings.explicit_lock_timeout_ms);
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

fn discover_roots(root_set: RootSet, settings: &Settings, cwd: &Path) -> Result<Vec<Root>> {
    let mut config = match root_set {
        RootSet::Publish => settings.publish.clone(),
        RootSet::Warm { .. } => settings.warm.clone(),
    };
    if let RootSet::Warm { checks } = root_set {
        config.checks |= checks;
    }
    if let Some(path) = env::var_os("WRIX_CACHE_ROOTS_FILE") {
        let roots = roots_from_file(Path::new(&path))?;
        return Ok(filter_roots(roots, &config));
    }
    let system = current_system()?;
    let mut installables = Vec::new();
    if config.packages {
        installables.extend(attr_installables("packages", &system, cwd)?);
    }
    if config.checks {
        installables.extend(attr_installables("checks", &system, cwd)?);
    }
    if config.dev_shell
        && let Some(shell) = selected_dev_shell()?
    {
        let installable = format!(".#devShells.{system}.{shell}");
        if drv_path(&installable, cwd).is_ok() {
            installables.push((format!("devShells.{system}.{shell}"), installable));
        }
    }
    for (index, installable) in config.include_roots.iter().enumerate() {
        installables.push((format!("include.{index}"), installable.clone()));
    }
    let mut roots = Vec::new();
    for (name, installable) in installables {
        let drv_path = drv_path(&installable, cwd)?;
        roots.push(Root {
            name,
            installable,
            drv_path,
            out_paths: Vec::new(),
        });
    }
    Ok(filter_roots(roots, &config))
}

fn filter_roots(roots: Vec<Root>, config: &Roots) -> Vec<Root> {
    let excludes: BTreeSet<&str> = config.exclude_roots.iter().map(String::as_str).collect();
    roots
        .into_iter()
        .filter(|root| root_category_enabled(root, config))
        .filter(|root| {
            !excludes.contains(root.name.as_str()) && !excludes.contains(root.installable.as_str())
        })
        .collect()
}

fn root_category_enabled(root: &Root, config: &Roots) -> bool {
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

fn roots_from_file(path: &Path) -> Result<Vec<Root>> {
    let content = read_document(path)?.ok_or_else(|| io::Error::from(ErrorKind::NotFound))?;
    roots_from_bytes(path, &content)
}

fn roots_from_bytes(path: &Path, content: &[u8]) -> Result<Vec<Root>> {
    let manifest =
        serde_json::from_slice::<RootManifest>(content).map_err(|source| Error::Json {
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

fn attr_installables(kind: &str, system: &str, cwd: &Path) -> Result<Vec<(String, String)>> {
    let attr = format!(".#{}.{system}", kind);
    let output = Command::new(nix_bin()?)
        .current_dir(cwd)
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

fn drv_path(installable: &str, cwd: &Path) -> Result<String> {
    let output = Command::new(nix_bin()?)
        .current_dir(cwd)
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

fn realized_roots(roots: Vec<Root>, cwd: &Path) -> Result<RealizedRoots> {
    let mut realized = Vec::new();
    let mut unrealized = Vec::new();
    for mut root in roots {
        if root.out_paths.is_empty() {
            if let Some(paths) = output_paths(&root.installable, cwd)? {
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    Ok(runtime.block_on(crate::backend::paths_realized(paths))?)
}

fn output_paths(installable: &str, cwd: &Path) -> Result<Option<Vec<String>>> {
    let output = Command::new(nix_bin()?)
        .current_dir(cwd)
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
        .iter()
        .filter(|(path, info)| path.starts_with("/nix/store/") && !info.is_null())
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn build_roots(roots: &[Root], cwd: &Path) -> Result<()> {
    if roots.is_empty() {
        return Ok(());
    }
    let mut command = Command::new(nix_bin()?);
    command.current_dir(cwd).arg("build").arg("--no-link");
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
    workspace: &WorkspaceHash,
    roots: &[Root],
    pending: Vec<Pending>,
) -> Result<Report> {
    paths.authorize()?;
    let root_drvs = roots
        .iter()
        .map(|root| root.drv_path.as_str())
        .collect::<HashSet<_>>();
    let mut publishable = BTreeSet::new();
    let mut lines = Vec::new();
    for root in roots {
        let closure = closure_paths(&root.out_paths)?;
        publishable.extend(closure);
        lines.push(format!("published root: {}", root.installable));
    }
    let mut completed_pending = Vec::new();
    for record in pending {
        if root_drvs.contains(record.drv_path.as_str()) {
            for path in closure_paths(&record.out_paths)? {
                publishable.insert(path);
            }
            completed_pending.push(record.path);
        }
    }
    let filtered = subtract_upstream(paths, publishable)?;
    let count = publish_selected(paths, workspace, &filtered)?;
    for path in completed_pending {
        fs::remove_file(&path)?;
        lines.push(format!("drained pending: {}", path.display()));
    }
    write_cache_status(paths, true, Some("ok"), None)?;
    lines.push(format!(
        "published {count} project cache paths through Celler"
    ));
    Ok(Report { lines })
}

fn closure_paths(out_paths: &[String]) -> Result<Vec<String>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    Ok(runtime.block_on(crate::backend::realized_closure(out_paths))?)
}

fn subtract_upstream(paths: &Paths, candidates: BTreeSet<String>) -> Result<Vec<String>> {
    let substituters = upstream_substituters(paths)?;
    let mut filtered = candidates;
    for substituter in &substituters {
        let remaining = filtered.iter().cloned().collect::<Vec<_>>();
        for batch in remaining.chunks(256) {
            for path in substitutable_paths(substituter, batch)? {
                filtered.remove(&path);
            }
        }
    }
    Ok(filtered.into_iter().collect())
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

fn substitutable_paths(substituter: &str, paths: &[String]) -> Result<Vec<String>> {
    let output = Command::new(nix_bin()?)
        .arg("path-info")
        .arg("--store")
        .arg(substituter)
        .arg("--json")
        .args(paths)
        .stdin(Stdio::null())
        .output()?;
    if output.stdout.is_empty() && !output.status.success() {
        tracing::warn!(
            path_count = paths.len(),
            "upstream lookup failed; retaining candidates for publication"
        );
        return Ok(Vec::new());
    }
    // Nix can return successful entries alongside null misses or a nonzero status.
    parse_path_info_paths(&String::from_utf8_lossy(&output.stdout))
}

fn publish_selected(
    paths: &Paths,
    workspace: &WorkspaceHash,
    store_paths: &[String],
) -> Result<usize> {
    let publisher = crate::backend::publication::Publisher::load(&paths.state_root, workspace)?;
    let selected = store_paths.iter().map(PathBuf::from).collect::<Vec<_>>();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    Ok(runtime.block_on(publisher.publish_selected(&selected))?)
}

fn read_pending(paths: &Paths) -> Result<Vec<Pending>> {
    let mut pending = Vec::new();
    let (records, observer) = pending_records(paths)?;
    for (path, record) in records {
        if pending_expired(&path, &record, paths.settings.pending_retention_secs)? {
            if observer {
                remove_pending(&path)?;
            }
            continue;
        }
        let created_at_epoch = pending_created_at(&path, &record)?;
        pending.push(Pending {
            path,
            created_at_epoch,
            drv_path: record.drv_path,
            out_paths: record.out_paths,
        });
    }
    Ok(pending)
}

fn record_pending(paths: &Paths, drv_path: &str, out_paths: &[String]) -> Result<PathBuf> {
    paths.authorize()?;
    if !wrix_state::namespace::is_host_root(&paths.state_root)? {
        fs::create_dir_all(paths.pending_dir())?;
    }
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

fn pending_expired(path: &Path, record: &PendingRecord, retention_secs: u64) -> Result<bool> {
    let created_at = pending_created_at(path, record)?;
    Ok(now_epoch()?.saturating_sub(created_at) > retention_secs)
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
        ages.push(Duration::from_secs(
            now_epoch()?.saturating_sub(record.created_at_epoch),
        ));
    }
    let pending_age = ages
        .into_iter()
        .max()
        .map_or_else(|| String::from("none"), |age| format!("{}s", age.as_secs()));
    let cache_size = directory_size(&paths.cache_root)?;
    let status = read_cache_status(paths)?;
    let endpoints = read_endpoints_text(paths)?;
    let manifest_path = paths.publish_roots_path();
    let publication_roots = match read_document(&manifest_path)? {
        Some(bytes) => roots_from_bytes(&manifest_path, &bytes)?.len(),
        None => 0,
    };
    let mut lines = vec![
        format!("cache size: {cache_size} bytes"),
        format!("pending records: {}", pending.len()),
        format!("publication roots: {publication_roots}"),
        format!("oldest pending age: {pending_age}"),
        format!("dirty: {}", cache_status_dirty(paths)?),
        format!(
            "last_publish: {}",
            status.last_publish.as_deref().unwrap_or("null")
        ),
        format!(
            "last_error: {}",
            status.last_error.as_deref().unwrap_or("null")
        ),
        format!("endpoints: {}", endpoints.trim()),
    ];
    let threshold = paths.settings.soft_limit_bytes;
    if cache_size > threshold {
        lines.push(format!(
            "warning: project cache size exceeds {threshold} byte soft threshold"
        ));
    }
    Ok(Report { lines })
}

fn rotate_key(paths: &Paths, workspace: &WorkspaceHash) -> Result<Report> {
    let publisher = crate::backend::publication::Publisher::load(&paths.state_root, workspace)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let key = runtime.block_on(publisher.rotate())?;
    record_rotated_key(paths, &key).map_err(|source| Error::RotationRecording {
        source: Box::new(source),
    })?;
    Ok(Report {
        lines: vec![String::from(
            "rotated project cache key and invalidated its Celler view; republish to populate it",
        )],
    })
}

fn record_rotated_key(paths: &Paths, key: &cache_key::CachePublicKey) -> Result<()> {
    fs::create_dir_all(paths.keys_dir())?;
    let temporary = paths
        .keys_dir()
        .join(format!("cache.pub.{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    std::io::Write::write_all(&mut file, format!("{}\n", key.as_str()).as_bytes())?;
    file.sync_all()?;
    fs::rename(temporary, paths.cache_public_path())?;
    write_cache_status(paths, false, None, None)
}

impl Paths {
    fn authorize(&self) -> Result<()> {
        if let Some(context) = &self.scope {
            context.authorize_effects(
                &wrix_state::host::Host::discover().map_err(wrix_state::store::Error::from)?,
            )?;
        }
        Ok(())
    }
    fn for_workspace(hash: &WorkspaceHash, settings: Settings) -> Result<Self> {
        let roots = wrix_core::host_path::Roots::discover()?;
        let state_root = roots.workspace_state(hash);
        let cache_root = roots.workspace_cache(hash);
        Ok(Self {
            scope: None,
            state_root,
            cache_root,
            settings,
        })
    }

    fn ensure_control(&self) -> Result<()> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.state_root)?;
        fs::create_dir_all(self.pending_dir())?;
        write_if_missing(&self.cache_lock_path(), "")?;
        write_json_if_missing(&self.cache_status_path(), &cache_status(false))
    }

    fn cache_lock_path(&self) -> PathBuf {
        self.state_root.join("cache.lock")
    }

    fn cache_status_path(&self) -> PathBuf {
        self.state_root.join("cache-status.json")
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

    fn cache_public_path(&self) -> PathBuf {
        self.keys_dir().join("cache.pub")
    }
}

fn nix_bin() -> Result<String> {
    executable_from_env("WRIX_NIX_BIN", "nix")
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
    error: Option<&str>,
) -> Result<()> {
    paths.authorize()?;
    let prior = read_cache_status(paths)?;
    let status = CacheStatus {
        dirty,
        last_publish: publish.map(str::to_owned).or(prior.last_publish),
        last_prune: prior.last_prune,
        last_error: error.map(str::to_owned).or_else(|| {
            if publish.is_some() {
                None
            } else {
                prior.last_error
            }
        }),
        last_prune_epoch: prior.last_prune_epoch,
    };
    write_json(&paths.cache_status_path(), &status)
}

fn read_cache_status(paths: &Paths) -> Result<CacheStatus> {
    let path = paths.cache_status_path();
    let Some(content) = read_document(&path)? else {
        return Ok(cache_status(false));
    };
    serde_json::from_slice(&content).map_err(|source| Error::Json {
        path: path.display().to_string(),
        source,
    })
}

fn cache_status_dirty(paths: &Paths) -> Result<bool> {
    Ok(read_cache_status(paths)?.dirty)
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
    if matches!(
        wrix_state::namespace::write_at(path, content.as_bytes())?,
        wrix_state::namespace::Storage::Filesystem
    ) {
        fs::write(path, content)?;
    }
    Ok(())
}

fn write_json_if_missing(path: &Path, value: &impl Serialize) -> Result<()> {
    let content = serde_json::to_vec_pretty(value).map_err(|source| Error::Json {
        path: path.display().to_string(),
        source,
    })?;
    match wrix_state::namespace::ensure_at(path, &content)? {
        wrix_state::namespace::Source::Host(Some(_)) => (),
        wrix_state::namespace::Source::Host(None) => {
            return Err(wrix_state::store::Error::Unsupported.into());
        }
        wrix_state::namespace::Source::Observer => {
            if !path.exists() {
                write_json(path, value)?;
            }
        }
    }
    Ok(())
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
    Ok(read_document(&paths.services_path())?.map_or_else(
        || String::from("unavailable"),
        |bytes| String::from_utf8_lossy(&bytes).into_owned(),
    ))
}

fn read_document(path: &Path) -> Result<Option<Vec<u8>>> {
    match wrix_state::namespace::read_at(path)? {
        wrix_state::namespace::Source::Host(content) => Ok(content),
        wrix_state::namespace::Source::Observer => match fs::read(path) {
            Ok(content) => Ok(Some(content)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        },
    }
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
