use std::{fs, io, path::Path, process::Command};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const WORKSPACE_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PROJECT_DRV: &str = "/nix/store/project-root.drv";
const OTHER_DRV: &str = "/nix/store/other-root.drv";

#[test]
fn post_build_hook_scopes_publish_to_manifest_roots() -> TestResult {
    let fixture = tempfile::Builder::new()
        .prefix("post-build-hook")
        .tempdir()?;
    let state_root = fixture.path().join("state");
    let cache_root = fixture.path().join("cache");
    let workspace = fixture.path().join("workspace");
    let record_dir = fixture.path().join("records");
    fs::create_dir_all(&state_root)?;
    fs::create_dir_all(&cache_root)?;
    fs::create_dir_all(&workspace)?;
    fs::create_dir_all(&record_dir)?;
    make_world_writable(&record_dir)?;

    let manifest = fixture.path().join("publish-roots.json");
    fs::write(
        &manifest,
        format!(
            r#"{{"roots":[{{"name":"pkg","installable":".#pkg","drv_path":"{PROJECT_DRV}","out_paths":["/nix/store/project-out"]}}]}}"#
        ),
    )?;
    let workspace_script = workspace.join("publish-from-workspace");
    fs::write(
        &workspace_script,
        "#!/usr/bin/env bash\nset -euo pipefail\ntouch \"$WRIX_WORKSPACE_SCRIPT_RAN\"\n",
    )?;
    make_executable(&workspace_script)?;

    let publisher = fixture.path().join("publisher-helper");
    write_publisher_helper(&publisher)?;
    let skipped_record = record_dir.join("skipped");
    let skipped_marker = workspace.join("evil-ran-skipped");
    let skipped = run_hook(HookInvocation {
        state_root: &state_root,
        cache_root: &cache_root,
        manifest: &manifest,
        publisher: &publisher,
        uid: current_uid()?,
        gid: current_gid()?,
        drv_path: OTHER_DRV,
        out_paths: "/nix/store/other-out",
        record_path: &skipped_record,
        workspace_marker: &skipped_marker,
    })?;
    assert!(skipped.status.success());
    assert!(String::from_utf8_lossy(&skipped.stdout).contains("skipping non-project derivation"));
    assert!(!skipped_record.exists());
    assert!(!skipped_marker.exists());

    let publish_uid = publish_owner_uid()?;
    let matched_record = record_dir.join("matched");
    let matched_marker = workspace.join("evil-ran-matched");
    let matched = run_hook(HookInvocation {
        state_root: &state_root,
        cache_root: &cache_root,
        manifest: &manifest,
        publisher: &publisher,
        uid: publish_uid,
        gid: publish_owner_gid()?,
        drv_path: PROJECT_DRV,
        out_paths: "/nix/store/project-out",
        record_path: &matched_record,
        workspace_marker: &matched_marker,
    })?;
    assert!(
        matched.status.success(),
        "{}",
        String::from_utf8_lossy(&matched.stderr)
    );
    let record = fs::read_to_string(matched_record)?;
    assert!(record.contains(&format!("uid={publish_uid}")));
    assert!(record.contains(&format!("--drv-path {PROJECT_DRV}")));
    assert!(record.contains("--out-paths /nix/store/project-out"));
    assert!(!matched_marker.exists());

    Ok(())
}

#[derive(Clone, Copy)]
struct HookInvocation<'a> {
    state_root: &'a Path,
    cache_root: &'a Path,
    manifest: &'a Path,
    publisher: &'a Path,
    uid: u32,
    gid: u32,
    drv_path: &'a str,
    out_paths: &'a str,
    record_path: &'a Path,
    workspace_marker: &'a Path,
}

fn run_hook(invocation: HookInvocation<'_>) -> io::Result<std::process::Output> {
    Command::new(env!("CARGO_BIN_EXE_wrix-cache-hook"))
        .arg("--workspace-hash")
        .arg(WORKSPACE_HASH)
        .arg("--owner-uid")
        .arg(invocation.uid.to_string())
        .arg("--owner-gid")
        .arg(invocation.gid.to_string())
        .arg("--state-root")
        .arg(invocation.state_root)
        .arg("--cache-root")
        .arg(invocation.cache_root)
        .arg("--manifest")
        .arg(invocation.manifest)
        .arg("--publisher-helper")
        .arg(invocation.publisher)
        .env("DRV_PATH", invocation.drv_path)
        .env("OUT_PATHS", invocation.out_paths)
        .env("WRIX_PUBLISH_RECORD", invocation.record_path)
        .env("WRIX_WORKSPACE_SCRIPT_RAN", invocation.workspace_marker)
        .output()
}

fn write_publisher_helper(path: &Path) -> io::Result<()> {
    fs::write(
        path,
        r#"#!/usr/bin/env bash
set -euo pipefail

{
  printf 'uid=%s\n' "$(id -u)"
  printf 'args='
  printf '%s ' "$@"
  printf '\n'
} >"$WRIX_PUBLISH_RECORD"
"#,
    )?;
    make_executable(path)
}

fn current_uid() -> io::Result<u32> {
    numeric_id("-u")
}

fn current_gid() -> io::Result<u32> {
    numeric_id("-g")
}

fn numeric_id(flag: &str) -> io::Result<u32> {
    let output = Command::new("id").arg(flag).output()?;
    if !output.status.success() {
        return Err(io::Error::other(String::from_utf8_lossy(&output.stderr)));
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .map_err(io::Error::other)
}

fn publish_owner_uid() -> io::Result<u32> {
    let current = current_uid()?;
    if current == 0 {
        Ok(65_534)
    } else {
        Ok(current)
    }
}

fn publish_owner_gid() -> io::Result<u32> {
    current_gid()
}

#[cfg(unix)]
fn make_executable(path: &Path) -> io::Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn make_world_writable(path: &Path) -> io::Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o777);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn make_world_writable(_path: &Path) -> io::Result<()> {
    Ok(())
}
