//! Native Nix hook/root-lifetime tests; the remote upstream alone is a subprocess fixture.
#![cfg(test)]
use fs2::FileExt;
use serde_json::json;
use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};
use wrix_cache::{
    backend::publication::Publisher,
    config::Settings,
    publisher::{self, HookRecord, Mode},
};
use wrix_core::path::Workspace;

#[path = "common/celler/mod.rs"]
#[expect(
    dead_code,
    reason = "this fixture uses only native isolated-store setup, not Celler HTTP helpers"
)]
mod native;

#[test]
#[ignore = "requires native disposable Nix daemon via unshare, no Celler server"]
fn selected_native_build_pins_build_only_dependencies_inside_post_build_hook() {
    if !native::in_isolated_store(
        "selected_native_build_pins_build_only_dependencies_inside_post_build_hook",
    ) {
        return;
    }
    run_native(false, false);
}

#[test]
#[ignore = "requires native disposable Nix daemon via unshare, no Celler server"]
fn failed_native_build_retains_protected_dependencies_without_publication() {
    if !native::in_isolated_store(
        "failed_native_build_retains_protected_dependencies_without_publication",
    ) {
        return;
    }
    run_native(true, false);
}

#[test]
#[ignore = "requires native disposable Nix daemon via unshare, no Celler server"]
fn substitution_only_native_warm_reconciles_without_post_build_hook() {
    if !native::in_isolated_store(
        "substitution_only_native_warm_reconciles_without_post_build_hook",
    ) {
        return;
    }
    run_native(false, true);
}

#[test]
#[ignore = "requires native disposable Nix daemon via unshare, no Celler server"]
fn automatic_timeout_persists_exact_native_roots_past_pending_ttl() {
    if !native::in_isolated_store("automatic_timeout_persists_exact_native_roots_past_pending_ttl")
    {
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let workspace = Workspace::from_path(root).unwrap();
    let state = root.join("state");
    let output = native::add_file("pending native output", "publisher-pending");
    Publisher::install(
        &state,
        workspace.hash(),
        "http://127.0.0.1:1/",
        attic::cache::CacheName::new("private".into()).unwrap(),
        "fixture",
        std::time::Duration::from_secs(3600),
    )
    .unwrap();
    let manifest = state.join("publish-roots.json");
    fs::write(&manifest, json!({"roots":[{"name":"packages.selected","installable":".#selected","drv_path":"/nix/store/selected.drv","out_paths":[output]}]}).to_string()).unwrap();
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state.join("cache.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    let settings = Settings {
        lock_timeout_ms: 0,
        pending_retention_secs: 0,
        ..Settings::default()
    };
    let report = publisher::run_hook_record(
        HookRecord {
            workspace_hash: workspace.hash(),
            state_root: &state,
            cache_root: &root.join("unused"),
            manifest: &manifest,
            drv_path: "/nix/store/selected.drv",
            out_paths: output.to_str().unwrap(),
        },
        settings.clone(),
    )
    .unwrap();
    assert!(
        report
            .lines()
            .iter()
            .any(|line| line.contains("recorded pending"))
    );
    let record = fs::read_dir(state.join("pending"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let filename = record.file_stem().unwrap().to_str().unwrap();
    let directory = state
        .join("publication-roots")
        .join(filename.strip_prefix("protected-").unwrap());
    assert_eq!(fs::read_link(directory.join("0")).unwrap(), output);
    let mut document: serde_json::Value =
        serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    document["created_at_epoch"] = json!(1);
    fs::write(&record, document.to_string()).unwrap();
    let status = publisher::status_at(&state, &root.join("unused"), settings).unwrap();
    assert!(
        status
            .lines()
            .iter()
            .any(|line| line == "pending records: 1")
    );
    assert!(record.exists());
    let gc = Command::new("nix-store")
        .arg("--delete")
        .arg(&output)
        .output()
        .unwrap();
    assert!(!gc.status.success());
    assert!(output.exists());
    lock.unlock().unwrap();
}

#[test]
fn disabled_hook_never_loads_authority_or_protects_outputs() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = Workspace::from_path(temporary.path()).unwrap();
    let state = temporary.path().join("missing");
    let report = publisher::run_hook_record(
        HookRecord {
            workspace_hash: workspace.hash(),
            state_root: &state,
            cache_root: &state,
            manifest: &state.join("missing.json"),
            drv_path: "/nix/store/selected.drv",
            out_paths: "/nix/store/selected",
        },
        Settings {
            enable: false,
            ..Settings::default()
        },
    )
    .unwrap();
    assert_eq!(report.lines(), ["project cache integration disabled"]);
    assert!(!state.exists());
}

fn run_native(fail: bool, substitute: bool) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let workspace = Workspace::from_path(root).unwrap();
    let state = root.join("state");
    Publisher::install(
        &state,
        workspace.hash(),
        "http://127.0.0.1:1/",
        attic::cache::CacheName::new("private".into()).unwrap(),
        "fixture",
        std::time::Duration::from_secs(3600),
    )
    .unwrap();
    let expression = format!(
        r#"let dep = derivation {{ name = "publisher-build-only"; system = builtins.currentSystem; builder = "/bin/sh"; args = [ "-c" "echo dependency > $out" ]; }}; in derivation {{ name = "publisher-selected"; system = builtins.currentSystem; builder = "/bin/sh"; dependency = dep; args = [ "-c" "{}" ]; }}"#,
        if fail {
            "exit 1"
        } else {
            "echo selected > $out"
        }
    );
    let instantiated = Command::new("nix-instantiate")
        .args(["--expr", &expression])
        .output()
        .unwrap();
    assert!(instantiated.status.success(), "{:?}", instantiated.stderr);
    let drv = String::from_utf8(instantiated.stdout)
        .unwrap()
        .trim()
        .to_owned();
    let roots = root.join("roots.json");
    fs::write(&roots, json!({"roots":[{"name":"packages.selected","installable":format!("{drv}^*"),"drv_path":drv,"out_paths":[]}]}).to_string()).unwrap();
    let fixture = json!({"state": state, "roots": roots, "workspace":root, "exe":env::current_exe().unwrap(), "fail":fail});
    fs::write(root.join("fixture.json"), fixture.to_string()).unwrap();
    fs::write(root.join("nix"), NIX).unwrap();
    fs::set_permissions(root.join("nix"), fs::Permissions::from_mode(0o755)).unwrap();
    let source = root.join("hook-source");
    fs::create_dir_all(source.join("bin")).unwrap();
    fs::write(source.join("fixture.json"), fixture.to_string()).unwrap();
    let hook = source.join("bin/wrix-cache-post-build-hook");
    fs::write(&hook, HOOK).unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let added = Command::new("nix-store")
        .arg("--add")
        .arg(&source)
        .output()
        .unwrap();
    assert!(added.status.success());
    let hook_store = String::from_utf8(added.stdout).unwrap().trim().to_owned();
    fs::write(
        root.join("hook-path"),
        format!("{hook_store}/bin/wrix-cache-post-build-hook"),
    )
    .unwrap();
    let run = || {
        Command::new(env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "native_publisher_child",
                "--nocapture",
            ])
            .current_dir(root)
            .env("WRIX_NATIVE_PUBLISHER_FIXTURE", root.join("fixture.json"))
            .env("WRIX_NATIVE_PUBLISHER_ACTION", "warm")
            .env("WRIX_CACHE_ROOTS_FILE", &roots)
            .env("WRIX_NIX_BIN", root.join("nix"))
            .env("WRIX_UPSTREAM_SUBSTITUTERS", "https://fixture.invalid")
            .output()
            .unwrap()
    };
    let result = run();
    assert_eq!(
        result.status.success(),
        !fail,
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let evidence: Vec<String> = fs::read_to_string(root.join("hook-evidence"))
        .unwrap_or_else(|error| {
            panic!(
                "missing hook evidence: {error}; {}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            )
        })
        .lines()
        .map(str::to_owned)
        .collect();
    assert!(
        evidence
            .iter()
            .any(|path| path.ends_with("publisher-build-only"))
    );
    let pending = fs::read_dir(state.join("pending"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    if fail {
        assert!(!pending.is_empty());
        assert!(
            pending
                .iter()
                .all(|entry| entry.file_name().as_encoded_bytes().starts_with(b"held-"))
        );
        let unprotected =
            native::add_file("unprotected publication control", "publisher-gc-control");
        let removed = Command::new("nix-store")
            .arg("--delete")
            .arg(&unprotected)
            .output()
            .unwrap();
        assert!(
            removed.status.success(),
            "{}",
            String::from_utf8_lossy(&removed.stderr)
        );
        assert!(!unprotected.exists());
        for path in evidence {
            let gc = Command::new("nix-store")
                .arg("--delete")
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                !gc.status.success(),
                "native GC must refuse protected output {path}"
            );
            assert!(Path::new(&path).exists(), "lost protected output {path}");
        }
    } else {
        assert!(pending.is_empty());
        assert_eq!(
            fs::read_dir(state.join("publication-roots"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            fs::read_dir(state.join("realization-roots"))
                .unwrap()
                .count(),
            0
        );
        if substitute {
            let outputs = Command::new("nix-store")
                .args(["--query", "--outputs", &drv])
                .output()
                .unwrap();
            assert!(outputs.status.success());
            let output = String::from_utf8(outputs.stdout).unwrap().trim().to_owned();
            let upstream = format!("file://{}", root.join("upstream").display());
            let generated = Command::new("nix-store")
                .arg("--generate-binary-cache-key")
                .arg("publisher-native")
                .arg(root.join("upstream.key"))
                .arg(root.join("upstream.pub"))
                .output()
                .unwrap();
            assert!(generated.status.success());
            let signed = Command::new("nix")
                .args(["store", "sign", "--key-file"])
                .arg(root.join("upstream.key"))
                .arg(&output)
                .output()
                .unwrap();
            assert!(
                signed.status.success(),
                "{}",
                String::from_utf8_lossy(&signed.stderr)
            );
            let copied = Command::new("nix")
                .args(["copy", "--to", &upstream, &output])
                .output()
                .unwrap();
            assert!(
                copied.status.success(),
                "{}",
                String::from_utf8_lossy(&copied.stderr)
            );
            let removed = Command::new("nix-store")
                .args(["--delete", &output])
                .output()
                .unwrap();
            assert!(
                removed.status.success(),
                "{}",
                String::from_utf8_lossy(&removed.stderr)
            );
            assert!(!Path::new(&output).exists());
            fs::remove_file(root.join("hook-evidence")).unwrap();
            let substituted = run();
            assert!(
                substituted.status.success(),
                "{}{}",
                String::from_utf8_lossy(&substituted.stdout),
                String::from_utf8_lossy(&substituted.stderr)
            );
            assert!(Path::new(&output).exists());
            assert!(
                !root.join("hook-evidence").exists(),
                "substitution must not run the native post-build hook: {}",
                fs::read_to_string(root.join("build-log")).unwrap()
            );
            assert_eq!(fs::read_dir(state.join("pending")).unwrap().count(), 0);
            assert_eq!(
                fs::read_dir(state.join("publication-roots"))
                    .unwrap()
                    .count(),
                0
            );
        }
    }
}

#[test]
#[ignore = "isolated subprocess helper"]
fn native_publisher_child() -> Result<(), Box<dyn std::error::Error>> {
    let fixture: serde_json::Value = serde_json::from_slice(&fs::read(
        env::var_os("WRIX_NATIVE_PUBLISHER_FIXTURE").unwrap(),
    )?)?;
    let state = PathBuf::from(fixture["state"].as_str().unwrap());
    let workspace = Workspace::from_path(fixture["workspace"].as_str().unwrap())?;
    let cache = state.join("unused");
    let report = if env::var("WRIX_NATIVE_PUBLISHER_ACTION")? == "hook" {
        publisher::run_hook_record(
            HookRecord {
                workspace_hash: workspace.hash(),
                state_root: &state,
                cache_root: &cache,
                manifest: &state.join("publish-roots.json"),
                drv_path: &env::var("DRV_PATH")?,
                out_paths: &env::var("OUT_PATHS")?,
            },
            Settings::default(),
        )?
    } else {
        publisher::run_workspace_at(&workspace, &state, &cache, Mode::Warm { checks: false })?
    };
    eprintln!("{:?}", report.lines());
    Ok(())
}

const NIX: &str = r"#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
root = pathlib.Path(__file__).parent
args = sys.argv[1:]
if args[:2] == ['path-info', '--store']:
    assert args[2:4] == ['https://fixture.invalid', '--json']
    print(json.dumps({path: {} for path in args[4:]}))
    sys.exit(0)
if args[0] == 'build' or args == ['config', 'show', 'post-build-hook']:
    args += ['--option', 'post-build-hook', (root / 'hook-path').read_text()]
    if (root / 'upstream').exists():
        args += ['--option', 'substituters', (root / 'upstream').as_uri(), '--option', 'trusted-public-keys', (root / 'upstream.pub').read_text().strip()]
result = subprocess.run(['nix'] + args, stderr=subprocess.PIPE)
if args[0] == 'build':
    (root / 'build-log').write_bytes(result.stderr)
sys.stderr.buffer.write(result.stderr)
sys.exit(result.returncode)
";

const HOOK: &str = r"#!/usr/bin/env python3
import json, os, pathlib, subprocess
fixture = json.load(open(pathlib.Path(__file__).parent.parent / 'fixture.json'))
root = pathlib.Path(fixture['workspace'])
env = os.environ.copy()
env['WRIX_NATIVE_PUBLISHER_FIXTURE'] = str(root / 'fixture.json')
env['WRIX_NATIVE_PUBLISHER_ACTION'] = 'hook'
subprocess.run([fixture['exe'], '--ignored', '--exact', 'native_publisher_child', '--nocapture'], env=env, check=True)
protected = [path.resolve() for path in (pathlib.Path(fixture['state']) / 'publication-roots').glob('*/*')]
assert any(str(path).endswith('publisher-build-only') for path in protected)
with open(root / 'hook-evidence', 'a') as evidence:
    for path in protected:
        evidence.write(str(path) + '\n')
";
