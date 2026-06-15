use std::{
    collections::{BTreeSet, HashSet},
    env, fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use wrix_core::path::{Workspace, WorkspaceHash};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Publish,
    Warm { checks: bool },
    Prune,
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
    publish_roots(&paths, &[root], Vec::new(), false)
}

fn run(workspace: &Workspace, paths: &Paths, mode: Mode) -> io::Result<Report> {
    match mode {
        Mode::Publish => {
            let roots = discover_roots(RootSet::Publish)?;
            write_manifest(&paths.publish_roots_path(), workspace.hash(), &roots)?;
            let realized = realized_roots(roots)?;
            let pending = read_pending(paths)?;
            publish_roots(paths, &realized.roots, pending, true).map(|mut report| {
                report.lines.extend(realized.unrealized);
                report
            })
        }
        Mode::Warm { checks } => {
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
        }
        Mode::Prune => {
            prune(paths)?;
            Ok(Report {
                lines: vec![String::from("pruned project cache")],
            })
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RootSet {
    Publish,
    Warm { checks: bool },
}

fn discover_roots(root_set: RootSet) -> io::Result<Vec<Root>> {
    if let Some(path) = env::var_os("WRIX_CACHE_ROOTS_FILE") {
        let roots = roots_from_file(Path::new(&path))?;
        return Ok(filter_roots(roots, root_set));
    }
    let system = current_system()?;
    let mut installables = Vec::new();
    installables.extend(attr_installables("packages", &system)?);
    match root_set {
        RootSet::Publish => installables.extend(attr_installables("checks", &system)?),
        RootSet::Warm { checks: true } => {
            installables.extend(attr_installables("checks", &system)?);
        }
        RootSet::Warm { checks: false } => {}
    }
    if let Some(shell) = selected_dev_shell() {
        let installable = format!(".#devShells.{system}.{shell}");
        if drv_path(&installable).is_ok() {
            installables.push((format!("devShells.{system}.{shell}"), installable));
        }
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
    Ok(roots)
}

fn filter_roots(roots: Vec<Root>, root_set: RootSet) -> Vec<Root> {
    roots
        .into_iter()
        .filter(|root| match root_set {
            RootSet::Publish | RootSet::Warm { checks: true } => true,
            RootSet::Warm { checks: false } => !root.name.starts_with("checks."),
        })
        .collect()
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
    let reachable = marker_store_basenames(paths)?;
    if !paths.cache_root.exists() {
        return Ok(());
    }
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
    fs::write(paths.cache_status_path(), cache_status(false))
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
        write_if_missing(&self.cache_lock_path(), "")?;
        write_if_missing(&self.cache_status_path(), cache_status(false))?;
        write_if_missing(&self.cache_secret_path(), "wrix-cache:missing-secret\n")?;
        write_if_missing(&self.cache_public_path(), "wrix-cache:missing-public\n")?;
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

    fn cache_secret_path(&self) -> PathBuf {
        self.keys_dir().join("cache.secret")
    }

    fn cache_public_path(&self) -> PathBuf {
        self.keys_dir().join("cache.pub")
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
        "{{\n  \"dirty\": {},\n  \"last_publish\": null,\n  \"last_prune\": null,\n  \"last_error\": null\n}}\n",
        if dirty { "true" } else { "false" }
    )
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

fn validate_workspace_hash(hash: &str) -> io::Result<()> {
    if hash.len() == 16 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "workspace hash must be sixteen hexadecimal characters",
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
