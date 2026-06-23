use std::{
    collections::{BTreeSet, HashSet},
    env,
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;
use wrix_core::{
    cache_key,
    path::{Workspace, WorkspaceHash},
};

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

#[derive(Clone, Debug, Eq, PartialEq)]
struct Root {
    name: String,
    installable: String,
    drv_path: String,
    out_paths: Vec<String>,
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

pub fn run_current_workspace(mode: Mode) -> io::Result<Report> {
    let workspace = Workspace::from_current_dir()?;
    let paths = Paths::for_workspace(workspace.hash())?;
    paths.ensure()?;
    run(&workspace, &paths, mode)
}

pub fn status_current_workspace() -> io::Result<Report> {
    let workspace = Workspace::from_current_dir()?;
    let paths = Paths::for_workspace(workspace.hash())?;
    paths.ensure()?;
    status_report(&paths)
}

pub fn prune_stale_dirty(state_root: &Path, cache_root: &Path) -> io::Result<bool> {
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
) -> io::Result<Report> {
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

fn run(workspace: &Workspace, paths: &Paths, mode: Mode) -> io::Result<Report> {
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

fn run_automatic_publish(paths: &Paths, root: Root) -> io::Result<Report> {
    let mut lines = Vec::new();
    let timeout = hook_lock_timeout();
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

fn with_explicit_lock<T>(
    paths: &Paths,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    let mut lines = Vec::new();
    let timeout = explicit_lock_timeout();
    let Some(lock) = OperationLock::acquire(paths, Some(timeout), &mut lines)? else {
        return Err(io::Error::new(
            ErrorKind::TimedOut,
            format!(
                "timed out waiting for project cache lock {}",
                paths.cache_lock_path().display()
            ),
        ));
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
    ) -> io::Result<Option<Self>> {
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
                    if timeout.is_some_and(|limit| started.elapsed().is_ok_and(|age| age >= limit))
                    {
                        return Ok(None);
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn release(self) -> io::Result<()> {
        self.file.unlock()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RootSet {
    Publish,
    Warm { checks: bool },
}

fn discover_roots(root_set: RootSet) -> io::Result<Vec<Root>> {
    let config = RootConfig::from_env(root_set);
    if let Some(path) = env::var_os("WRIX_CACHE_ROOTS_FILE") {
        let roots = roots_from_file(Path::new(&path))?;
        return Ok(filter_roots(roots, &config));
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
        && let Some(shell) = selected_dev_shell()
    {
        let installable = format!(".#devShells.{system}.{shell}");
        if drv_path(&installable).is_ok() {
            installables.push((format!("devShells.{system}.{shell}"), installable));
        }
    }
    for (index, installable) in env_lines(config.include_env).into_iter().enumerate() {
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
    Ok(filter_roots(roots, &config))
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
    fn from_env(root_set: RootSet) -> Self {
        match root_set {
            RootSet::Publish => Self {
                packages: env_bool("WRIX_CACHE_PUBLISH_PACKAGES", true),
                checks: env_bool("WRIX_CACHE_PUBLISH_CHECKS", true),
                dev_shell: env_bool("WRIX_CACHE_PUBLISH_DEVSHELL", true),
                include_env: "WRIX_CACHE_PUBLISH_INCLUDE",
                exclude_env: "WRIX_CACHE_PUBLISH_EXCLUDE",
            },
            RootSet::Warm { checks } => Self {
                packages: env_bool("WRIX_CACHE_WARM_PACKAGES", true),
                checks: checks || env_bool("WRIX_CACHE_WARM_CHECKS", false),
                dev_shell: env_bool("WRIX_CACHE_WARM_DEVSHELL", true),
                include_env: "WRIX_CACHE_WARM_INCLUDE",
                exclude_env: "WRIX_CACHE_WARM_EXCLUDE",
            },
        }
    }
}

fn filter_roots(roots: Vec<Root>, config: &RootConfig) -> Vec<Root> {
    let excludes: BTreeSet<String> = env_lines(config.exclude_env).into_iter().collect();
    roots
        .into_iter()
        .filter(|root| root_category_enabled(root, config))
        .filter(|root| !excludes.contains(&root.name) && !excludes.contains(&root.installable))
        .collect()
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

fn env_bool(name: &str, default: bool) -> bool {
    match env::var(name) {
        Ok(value) if matches!(value.as_str(), "0" | "false" | "no") => false,
        Ok(value) if matches!(value.as_str(), "1" | "true" | "yes") => true,
        Ok(_) | Err(_) => default,
    }
}

fn env_lines(name: &str) -> Vec<String> {
    env::var(name).map_or_else(
        |_| Vec::new(),
        |value| {
            value
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect()
        },
    )
}

fn roots_from_file(path: &Path) -> io::Result<Vec<Root>> {
    let content = fs::read_to_string(path)?;
    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    for object in json_objects(&content) {
        if let (Some(name), Some(installable), Some(drv_path)) = (
            json_string_field(object, "name"),
            json_string_field(object, "installable"),
            json_string_field(object, "drv_path").or_else(|| json_string_field(object, "drvPath")),
        ) {
            if !seen.insert((name.clone(), drv_path.clone())) {
                continue;
            }
            roots.push(Root {
                name,
                installable,
                drv_path,
                out_paths: json_string_array_field(object, "out_paths")
                    .or_else(|| json_string_array_field(object, "outPaths"))
                    .unwrap_or_default(),
            });
        }
    }
    Ok(roots)
}

fn attr_installables(kind: &str, system: &str) -> io::Result<Vec<(String, String)>> {
    let attr = format!(".#{}.{system}", kind);
    let output = Command::new(nix_bin())
        .arg("eval")
        .arg("--json")
        .arg(&attr)
        .arg("--apply")
        .arg("builtins.attrNames")
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut installables = Vec::new();
    for name in parse_json_string_array(&text) {
        installables.push((
            format!("{kind}.{system}.{name}"),
            format!(".#{kind}.{system}.{name}"),
        ));
    }
    Ok(installables)
}

fn selected_dev_shell() -> Option<String> {
    match env::var("WRIX_DEVSHELL") {
        Ok(value) if value.is_empty() || value == "none" => None,
        Ok(value) => Some(value),
        Err(env::VarError::NotPresent) => Some(String::from("default")),
        Err(env::VarError::NotUnicode(_)) => None,
    }
}

fn current_system() -> io::Result<String> {
    if let Ok(system) = env::var("WRIX_SYSTEM") {
        return Ok(system);
    }
    let output = Command::new(nix_bin())
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
    Err(io::Error::other(
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

fn drv_path(installable: &str) -> io::Result<String> {
    let output = Command::new(nix_bin())
        .arg("path-info")
        .arg("--derivation")
        .arg(installable)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    Err(io::Error::other(
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

struct RealizedRoots {
    roots: Vec<Root>,
    unrealized: Vec<String>,
}

fn realized_roots(roots: Vec<Root>) -> io::Result<RealizedRoots> {
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
        realized.push(root);
    }
    Ok(RealizedRoots {
        roots: realized,
        unrealized,
    })
}

fn output_paths(installable: &str) -> io::Result<Option<Vec<String>>> {
    let output = Command::new(nix_bin())
        .arg("path-info")
        .arg("--json")
        .arg(installable)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let paths = parse_store_paths(&text);
    if paths.is_empty() {
        Ok(None)
    } else {
        Ok(Some(paths))
    }
}

fn build_roots(roots: &[Root]) -> io::Result<()> {
    if roots.is_empty() {
        return Ok(());
    }
    let mut command = Command::new(nix_bin());
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
        Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }
}

fn publish_roots(
    paths: &Paths,
    roots: &[Root],
    pending: Vec<Pending>,
    prune_after: bool,
) -> io::Result<Report> {
    let root_drvs = roots
        .iter()
        .map(|root| root.drv_path.as_str())
        .collect::<HashSet<_>>();
    let mut publishable = BTreeSet::new();
    let mut lines = Vec::new();
    for root in roots {
        update_gc_marker(paths, root)?;
        for path in closure_paths(&root.out_paths)? {
            publishable.insert(path);
        }
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

fn closure_paths(out_paths: &[String]) -> io::Result<Vec<String>> {
    if out_paths.is_empty() {
        return Ok(Vec::new());
    }
    let output = Command::new(nix_store_bin())
        .arg("--query")
        .arg("--requisites")
        .args(out_paths)
        .stdin(Stdio::null())
        .output()?;
    if output.status.success() {
        return Ok(split_paths(&String::from_utf8_lossy(&output.stdout)));
    }
    Ok(out_paths.to_vec())
}

fn subtract_upstream(paths: &Paths, candidates: BTreeSet<String>) -> io::Result<Vec<String>> {
    let substituters = upstream_substituters(paths)?;
    let mut filtered = Vec::new();
    for candidate in candidates {
        if substituters
            .iter()
            .any(|substituter| substitutable(substituter, &candidate))
        {
            continue;
        }
        filtered.push(candidate);
    }
    Ok(filtered)
}

fn upstream_substituters(paths: &Paths) -> io::Result<Vec<String>> {
    if let Ok(value) = env::var("WRIX_UPSTREAM_SUBSTITUTERS") {
        return Ok(value.split_whitespace().map(str::to_owned).collect());
    }
    let output = Command::new(nix_bin())
        .arg("config")
        .arg("show")
        .arg("substituters")
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Ok(Vec::new());
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

fn substitutable(substituter: &str, path: &str) -> bool {
    Command::new(nix_bin())
        .arg("path-info")
        .arg("--store")
        .arg(substituter)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn copy_to_cache(paths: &Paths, store_paths: &[String]) -> io::Result<()> {
    if store_paths.is_empty() {
        return Ok(());
    }
    let mut command = Command::new(nix_bin());
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
        Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }
}

fn prune(paths: &Paths) -> io::Result<()> {
    purge_expired_pending(paths)?;
    let reachable = marker_store_basenames(paths)?;
    if paths.cache_root.exists() {
        for entry in fs::read_dir(&paths.cache_root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("narinfo") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            if !reachable.iter().any(|base| base.starts_with(stem)) {
                fs::remove_file(path)?;
            }
        }
    }
    write_cache_status(paths, false, None, Some("ok"), None)
}

fn marker_store_basenames(paths: &Paths) -> io::Result<Vec<String>> {
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

fn update_gc_marker(paths: &Paths, root: &Root) -> io::Result<()> {
    let marker = paths.gcroots_dir().join(safe_marker_name(&root.name));
    fs::write(marker, format!("{}\n", root.out_paths.join("\n")))
}

fn read_pending(paths: &Paths) -> io::Result<Vec<Pending>> {
    let mut pending = Vec::new();
    if !paths.pending_dir().exists() {
        return Ok(pending);
    }
    for entry in fs::read_dir(paths.pending_dir())? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let content = fs::read_to_string(&path)?;
        if pending_expired(&path, &content)? {
            fs::remove_file(path)?;
            continue;
        }
        if let Some(drv_path) = json_string_field(&content, "drv_path")
            .or_else(|| json_string_field(&content, "drvPath"))
        {
            let out_paths = json_string_array_field(&content, "out_paths")
                .or_else(|| json_string_array_field(&content, "outPaths"))
                .unwrap_or_else(|| {
                    json_string_field(&content, "out_paths")
                        .map(|value| split_paths(&value))
                        .unwrap_or_default()
                });
            pending.push(Pending {
                path,
                drv_path,
                out_paths,
            });
        }
    }
    Ok(pending)
}

fn record_pending(paths: &Paths, drv_path: &str, out_paths: &[String]) -> io::Result<PathBuf> {
    fs::create_dir_all(paths.pending_dir())?;
    let filename = format!(
        "{}-{}-{}.json",
        now_epoch(),
        std::process::id(),
        safe_pending_name(drv_path)
    );
    let path = paths.pending_dir().join(filename);
    let content = format!(
        concat!(
            "{{\n",
            "  \"created_at_epoch\": {},\n",
            "  \"drv_path\": \"{}\",\n",
            "  \"out_paths\": [{}]\n",
            "}}\n"
        ),
        now_epoch(),
        escape_json(drv_path),
        json_string_array(out_paths)
    );
    fs::write(&path, content)?;
    Ok(path)
}

fn purge_expired_pending(paths: &Paths) -> io::Result<()> {
    if !paths.pending_dir().exists() {
        return Ok(());
    }
    for entry in fs::read_dir(paths.pending_dir())? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let content = fs::read_to_string(&path)?;
        if pending_expired(&path, &content)? {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn pending_expired(path: &Path, content: &str) -> io::Result<bool> {
    let Some(created_at) = pending_created_at(path, content)? else {
        return Ok(false);
    };
    Ok(now_epoch().saturating_sub(created_at) > pending_retention().as_secs())
}

fn pending_created_at(path: &Path, content: &str) -> io::Result<Option<u64>> {
    if let Some(value) = json_number_field(content, "created_at_epoch") {
        return Ok(Some(value));
    }
    let metadata = fs::metadata(path)?;
    let modified = metadata.modified()?;
    Ok(system_time_epoch(modified))
}

fn read_manifest_roots(path: &Path) -> io::Result<Vec<Root>> {
    roots_from_file(path)
}

fn write_manifest(path: &Path, hash: &WorkspaceHash, roots: &[Root]) -> io::Result<()> {
    let mut content = format!(
        "{{\n  \"schema_version\": 1,\n  \"workspace_hash\": \"{}\",\n  \"roots\": [\n",
        escape_json(hash.as_str())
    );
    for (index, root) in roots.iter().enumerate() {
        let comma = if index + 1 == roots.len() { "" } else { "," };
        content.push_str("    { \"name\": \"");
        content.push_str(&escape_json(&root.name));
        content.push_str("\", \"installable\": \"");
        content.push_str(&escape_json(&root.installable));
        content.push_str("\", \"drv_path\": \"");
        content.push_str(&escape_json(&root.drv_path));
        content.push_str("\", \"out_paths\": [");
        content.push_str(&json_string_array(&root.out_paths));
        content.push_str("] }");
        content.push_str(comma);
        content.push('\n');
    }
    content.push_str("  ]\n}\n");
    fs::write(path, content)
}

fn status_report(paths: &Paths) -> io::Result<Report> {
    let pending = read_pending(paths)?;
    let pending_age = pending
        .iter()
        .filter_map(|record| pending_file_age(&record.path))
        .max()
        .map_or_else(|| String::from("none"), |age| format!("{}s", age.as_secs()));
    let cache_size = directory_size(&paths.cache_root)?;
    let status = read_cache_status_text(paths)?;
    let endpoints = read_endpoints_text(paths)?;
    let mut lines = vec![
        format!("cache size: {cache_size} bytes"),
        format!("pending records: {}", pending.len()),
        format!("oldest pending age: {pending_age}"),
        format!("dirty: {}", cache_status_dirty(paths)?),
        format!(
            "last_publish: {}",
            json_nullable_string_field(&status, "last_publish")
        ),
        format!(
            "last_prune: {}",
            json_nullable_string_field(&status, "last_prune")
        ),
        format!(
            "last_error: {}",
            json_nullable_string_field(&status, "last_error")
        ),
        format!("endpoints: {}", endpoints.trim()),
    ];
    let threshold = soft_size_threshold();
    if cache_size > threshold {
        lines.push(format!(
            "warning: project cache size exceeds {threshold} byte soft threshold"
        ));
    }
    Ok(Report { lines })
}

fn rotate_key(paths: &Paths) -> io::Result<Report> {
    wipe_cache(paths)?;
    generate_project_keypair(paths)?;
    write_cache_status(paths, false, None, Some("key-rotated"), None)?;
    Ok(Report {
        lines: vec![String::from(
            "rotated project cache key and invalidated local cache",
        )],
    })
}

fn wipe_cache(paths: &Paths) -> io::Result<()> {
    if paths.cache_root.exists() {
        fs::remove_dir_all(&paths.cache_root)?;
    }
    fs::create_dir_all(paths.cache_root.join("nar"))?;
    fs::create_dir_all(paths.cache_root.join("log"))?;
    fs::write(
        paths.cache_root.join("nix-cache-info"),
        "StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 40\n",
    )
}

fn generate_project_keypair(paths: &Paths) -> io::Result<()> {
    cache_key::generate_keypair(
        &paths.key_name(),
        &paths.cache_secret_path(),
        &paths.cache_public_path(),
        &nix_store_bin(),
    )
}

impl Paths {
    fn for_workspace(hash: &WorkspaceHash) -> io::Result<Self> {
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

    fn ensure(&self) -> io::Result<()> {
        fs::create_dir_all(&self.state_root)?;
        fs::create_dir_all(&self.cache_root)?;
        fs::create_dir_all(self.gcroots_dir())?;
        fs::create_dir_all(self.pending_dir())?;
        fs::create_dir_all(self.keys_dir())?;
        fs::create_dir_all(self.cache_root.join("nar"))?;
        fs::create_dir_all(self.cache_root.join("log"))?;
        write_if_missing(&self.cache_lock_path(), "")?;
        write_if_missing(&self.cache_status_path(), cache_status(false))?;
        cache_key::ensure_keypair(
            &self.key_name(),
            &self.cache_secret_path(),
            &self.cache_public_path(),
            &nix_store_bin(),
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

fn home_dir() -> io::Result<PathBuf> {
    env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "HOME is required to resolve wrix cache state roots",
        )
    })
}

fn nix_bin() -> String {
    env::var("WRIX_NIX_BIN").unwrap_or_else(|_| String::from("nix"))
}

fn nix_store_bin() -> String {
    env::var("WRIX_NIX_STORE_BIN").unwrap_or_else(|_| String::from("nix-store"))
}

fn cache_status(dirty: bool) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"dirty\": {},\n",
            "  \"last_publish\": null,\n",
            "  \"last_prune\": null,\n",
            "  \"last_error\": null,\n",
            "  \"last_prune_epoch\": null\n",
            "}}\n"
        ),
        if dirty { "true" } else { "false" }
    )
}

fn write_cache_status(
    paths: &Paths,
    dirty: bool,
    publish: Option<&str>,
    prune_status: Option<&str>,
    error: Option<&str>,
) -> io::Result<()> {
    let prior = read_cache_status_text(paths)?;
    let last_publish = publish
        .map(str::to_owned)
        .or_else(|| json_string_field(&prior, "last_publish"));
    let last_prune = prune_status
        .map(str::to_owned)
        .or_else(|| json_string_field(&prior, "last_prune"));
    let last_error = error.map_or_else(
        || {
            if publish.is_some() || prune_status.is_some() {
                None
            } else {
                json_string_field(&prior, "last_error")
            }
        },
        |value| Some(value.to_owned()),
    );
    let prune_epoch = if prune_status.is_some() {
        Some(now_epoch())
    } else {
        json_number_field(&prior, "last_prune_epoch")
    };
    let content = format!(
        concat!(
            "{{\n",
            "  \"dirty\": {},\n",
            "  \"last_publish\": {},\n",
            "  \"last_prune\": {},\n",
            "  \"last_error\": {},\n",
            "  \"last_prune_epoch\": {}\n",
            "}}\n"
        ),
        if dirty { "true" } else { "false" },
        json_optional_string(last_publish.as_deref()),
        json_optional_string(last_prune.as_deref()),
        json_optional_string(last_error.as_deref()),
        prune_epoch.map_or_else(|| String::from("null"), |value| value.to_string())
    );
    fs::write(paths.cache_status_path(), content)
}

fn read_cache_status_text(paths: &Paths) -> io::Result<String> {
    match fs::read_to_string(paths.cache_status_path()) {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(cache_status(false)),
        Err(error) => Err(error),
    }
}

fn cache_status_dirty(paths: &Paths) -> io::Result<bool> {
    let content = read_cache_status_text(paths)?;
    Ok(json_bool_field(&content, "dirty").unwrap_or(false))
}

fn prune_is_stale(paths: &Paths) -> io::Result<bool> {
    let content = read_cache_status_text(paths)?;
    let Some(last_prune) = json_number_field(&content, "last_prune_epoch") else {
        return Ok(true);
    };
    Ok(now_epoch().saturating_sub(last_prune) > prune_interval().as_secs())
}

fn write_if_missing(path: &Path, content: impl AsRef<[u8]>) -> io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    fs::write(path, content)
}

fn split_paths(input: &str) -> Vec<String> {
    input
        .split_whitespace()
        .filter(|value| value.starts_with("/nix/store/"))
        .map(str::to_owned)
        .collect()
}

fn parse_store_paths(input: &str) -> Vec<String> {
    let mut paths = parse_json_string_array(input)
        .into_iter()
        .filter(|value| value.starts_with("/nix/store/"))
        .collect::<Vec<_>>();
    paths.extend(split_paths(input));
    paths.sort();
    paths.dedup();
    paths
}

fn parse_json_string_array(input: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut rest = input;
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('"') else {
            break;
        };
        values.push(after[..end].to_owned());
        rest = &after[end + 1..];
    }
    values
}

fn json_objects(input: &str) -> Vec<&str> {
    let mut objects = Vec::new();
    let mut starts = Vec::new();
    for (index, ch) in input.char_indices() {
        match ch {
            '{' => starts.push(index),
            '}' => {
                if let Some(object_start) = starts.pop() {
                    objects.push(&input[object_start..=index]);
                }
            }
            _ => {}
        }
    }
    objects
}

fn json_string_field(input: &str, name: &str) -> Option<String> {
    let marker = format!("\"{name}\"");
    let start = input.find(&marker)? + marker.len();
    let colon = input[start..].find(':')? + start;
    let rest = input[colon + 1..].trim_start();
    let value = rest.strip_prefix('"')?;
    let end = value.find('"')?;
    Some(value[..end].to_owned())
}

fn json_string_array_field(input: &str, name: &str) -> Option<Vec<String>> {
    let marker = format!("\"{name}\"");
    let start = input.find(&marker)? + marker.len();
    let colon = input[start..].find(':')? + start;
    let rest = input[colon + 1..].trim_start();
    let value = rest.strip_prefix('[')?;
    let end = value.find(']')?;
    Some(parse_json_string_array(&value[..end]))
}

fn json_number_field(input: &str, name: &str) -> Option<u64> {
    let marker = format!("\"{name}\"");
    let start = input.find(&marker)? + marker.len();
    let colon = input[start..].find(':')? + start;
    let rest = input[colon + 1..].trim_start();
    let digits = rest
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits.parse().ok()
}

fn json_bool_field(input: &str, name: &str) -> Option<bool> {
    let marker = format!("\"{name}\"");
    let start = input.find(&marker)? + marker.len();
    let colon = input[start..].find(':')? + start;
    let rest = input[colon + 1..].trim_start();
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

fn json_nullable_string_field(input: &str, name: &str) -> String {
    json_string_field(input, name).unwrap_or_else(|| String::from("null"))
}

fn json_optional_string(value: Option<&str>) -> String {
    value.map_or_else(
        || String::from("null"),
        |text| format!("\"{}\"", escape_json(text)),
    )
}

fn json_string_array(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("\"{}\"", escape_json(value)))
        .collect::<Vec<_>>()
        .join(", ")
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
    let base = Path::new(input)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("pending");
    safe_marker_name(base)
}

fn now_epoch() -> u64 {
    system_time_epoch(SystemTime::now()).unwrap_or(0)
}

fn system_time_epoch(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .ok()
}

fn pending_file_age(path: &Path) -> Option<Duration> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    modified.elapsed().ok()
}

fn hook_lock_timeout() -> Duration {
    env_duration("WRIX_CACHE_LOCK_TIMEOUT_MS", Duration::from_secs(30))
}

fn explicit_lock_timeout() -> Duration {
    env_duration(
        "WRIX_CACHE_EXPLICIT_LOCK_TIMEOUT_MS",
        env_duration("WRIX_CACHE_LOCK_TIMEOUT_MS", Duration::from_secs(30)),
    )
}

fn pending_retention() -> Duration {
    const DEFAULT_PENDING_RETENTION_SECS: u64 = 604_800;
    env_seconds(
        "WRIX_CACHE_PENDING_RETENTION_SECS",
        Duration::from_secs(DEFAULT_PENDING_RETENTION_SECS),
    )
}

fn prune_interval() -> Duration {
    const DEFAULT_PRUNE_INTERVAL_SECS: u64 = 86_400;
    env_seconds(
        "WRIX_CACHE_PRUNE_INTERVAL_SECS",
        Duration::from_secs(DEFAULT_PRUNE_INTERVAL_SECS),
    )
}

fn soft_size_threshold() -> u64 {
    env::var("WRIX_CACHE_SOFT_LIMIT_BYTES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(50 * 1024 * 1024 * 1024)
}

fn env_duration(name: &str, default: Duration) -> Duration {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(default, Duration::from_millis)
}

fn env_seconds(name: &str, default: Duration) -> Duration {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(default, Duration::from_secs)
}

fn directory_size(path: &Path) -> io::Result<u64> {
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

fn read_endpoints_text(paths: &Paths) -> io::Result<String> {
    match fs::read_to_string(paths.services_path()) {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(String::from("unavailable")),
        Err(error) => Err(error),
    }
}

fn validate_workspace_hash(hash: &str) -> io::Result<()> {
    if WorkspaceHash::is_valid_str(hash) {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "workspace hash must be a lowercase sha256 hex digest",
    ))
}

fn escape_json(input: &str) -> String {
    input.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod test {
    use super::{parse_store_paths, roots_from_file, safe_marker_name};
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
    fn extracts_store_paths_from_nix_json() {
        let paths = parse_store_paths(r#"{"/nix/store/aaa-root":{"path":"/nix/store/bbb-out"}}"#);
        assert_eq!(
            paths,
            vec![
                String::from("/nix/store/aaa-root"),
                String::from("/nix/store/bbb-out")
            ]
        );
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
