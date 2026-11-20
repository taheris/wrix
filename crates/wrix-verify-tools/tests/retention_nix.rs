#![cfg(test)]

use std::{
    fs,
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

const TOOL: &str = env!("CARGO_BIN_EXE_wrix-verification-roots");

fn success(command: &mut Command) -> Output {
    let output = command.output().unwrap();
    assert!(output.status.success(), "{command:?}: {output:?}");
    output
}

fn add(path: &Path) -> String {
    String::from_utf8(success(Command::new("nix-store").arg("--add").arg(path)).stdout)
        .unwrap()
        .trim()
        .to_owned()
}

#[test]
#[ignore = "requires a real writable Nix store; owned by prek.verification-root-retention"]
fn actual_registered_root_survives_caller_death_until_its_descendant_releases_the_lease() {
    let temporary = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let source = temporary.path().join("source");
    fs::write(&source, format!("lease {}", temporary.path().display())).unwrap();
    let source = add(&source);
    let cache = temporary.path().join("sessions");
    let mut child = Command::new(TOOL)
        .arg("session")
        .arg(&cache)
        .args([
            "--",
            "bash",
            "-c",
            r#"set -euo pipefail
nix-store --add-root "$WRIX_CI_BATCH_DIR/runner" --indirect --realise "$LEASE_SOURCE" > /dev/null
sleep 5 &
printf '%s\n' "$WRIX_CI_BATCH_DIR"
wait
"#,
        ])
        .env("LEASE_SOURCE", &source)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut batch = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut batch)
        .unwrap();
    let batch = PathBuf::from(batch.trim());
    child.kill().unwrap();
    child.wait().unwrap();
    success(Command::new(TOOL).arg("session-clean").arg(&cache));
    assert!(batch.join("runner").is_symlink());
    let roots = success(Command::new("nix-store").args(["--query", "--roots", &source]));
    assert!(
        String::from_utf8(roots.stdout)
            .unwrap()
            .contains(batch.to_str().unwrap())
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while batch.exists() {
        assert!(Instant::now() < deadline);
        success(Command::new(TOOL).arg("session-clean").arg(&cache));
        std::thread::sleep(Duration::from_millis(20));
    }
    let roots = success(Command::new("nix-store").args(["--query", "--roots", &source]));
    assert!(
        !String::from_utf8(roots.stdout)
            .unwrap()
            .contains(batch.to_str().unwrap())
    );
}

#[test]
#[ignore = "requires a real writable Nix store; owned by prek.verification-root-retention"]
fn pending_frontier_registers_only_available_outputs_and_prunes_completed_producer_inputs() {
    let temporary = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let directory = temporary.path();
    let source = directory.join("source");
    fs::write(&source, format!("frontier {}", directory.display())).unwrap();
    let source = add(&source);
    let bash = String::from_utf8(
        success(Command::new("bash").args(["-c", "printf '%s' \"$BASH\""])).stdout,
    )
    .unwrap();
    assert!(bash.starts_with("/nix/store/"), "{bash}");
    let system = String::from_utf8(
        success(Command::new("nix").args([
            "eval",
            "--impure",
            "--raw",
            "--expr",
            "builtins.currentSystem",
        ]))
        .stdout,
    )
    .unwrap();
    let boundary = directory.file_name().unwrap().to_str().unwrap();
    let expression = format!(
        r#"let
      builder = builtins.storePath {bash};
      make = name: src: derivation {{
        inherit name src builder;
        system = {system};
        args = [ "-c" ''content=$(< "$src"); printf '%s' "$content" > "$out"'' ];
      }};
      leaf = make "wrix-frontier-leaf-{boundary}" (builtins.storePath {source});
      root = make "wrix-frontier-root-{boundary}" leaf;
    in {{ inherit leaf root; }}"#,
        bash = serde_json::to_string(&bash).unwrap(),
        system = serde_json::to_string(&system).unwrap(),
        source = serde_json::to_string(&source).unwrap()
    );
    let root = String::from_utf8(
        success(
            Command::new("nix-instantiate")
                .arg("--add-root")
                .arg(directory.join("root-recipe"))
                .arg("--indirect")
                .args(["--expr", &expression, "--attr", "root"]),
        )
        .stdout,
    )
    .unwrap();
    let root = fs::read_link(root.trim()).unwrap();
    let leaf = String::from_utf8(
        success(Command::new("nix-instantiate").args(["--expr", &expression, "--attr", "leaf"]))
            .stdout,
    )
    .unwrap();
    let script = directory.join("observe.sh");
    fs::write(&script, r#"set -euo pipefail
frontier() {
  "$ROOT_MANAGER" session-frontier "$WRIX_CI_BATCH_DIR" "$WRIX_CI_BATCH_LEASE_FD" "$BASH" nix nix-store -- "$ROOT_RECIPE"
}
inventory() {
  local name="$1" current
  current=$(readlink "$WRIX_CI_BATCH_DIR/pending/current")
  cp "$WRIX_CI_BATCH_DIR/pending/$current/manifest" "$OBSERVATIONS/$name"
}
printf '%s' "$WRIX_CI_BATCH_DIR" > "$OBSERVATIONS/batch"
frontier
inventory before
nix-store --realise "$LEAF_RECIPE" > "$OBSERVATIONS/leaf-output"
frontier
inventory middle
nix-store --realise "$ROOT_RECIPE" > "$OBSERVATIONS/root-output"
frontier
inventory after
"#).unwrap();
    let cache = directory.join("sessions");
    success(
        Command::new(TOOL)
            .arg("session")
            .arg(&cache)
            .args(["--", &bash])
            .arg(&script)
            .env("ROOT_MANAGER", TOOL)
            .env("ROOT_RECIPE", &root)
            .env("LEAF_RECIPE", leaf.trim())
            .env("OBSERVATIONS", directory),
    );
    let before = fs::read_to_string(directory.join("before")).unwrap();
    let middle = fs::read_to_string(directory.join("middle")).unwrap();
    let after = fs::read_to_string(directory.join("after")).unwrap();
    let leaf = fs::read_to_string(directory.join("leaf-output")).unwrap();
    let root = fs::read_to_string(directory.join("root-output")).unwrap();
    assert!(before.lines().any(|path| path == source));
    assert!(middle.lines().any(|path| path == leaf.trim()));
    assert!(!middle.lines().any(|path| path == source));
    assert_eq!(after, root);
    let batch = fs::read_to_string(directory.join("batch")).unwrap();
    let roots = success(Command::new("nix-store").args(["--query", "--roots", root.trim()]));
    assert!(String::from_utf8(roots.stdout).unwrap().contains(&batch));
    success(Command::new(TOOL).arg("session-clean").arg(cache));
    assert!(!Path::new(&batch).exists());
    let roots = success(Command::new("nix-store").args(["--query", "--roots", root.trim()]));
    assert!(!String::from_utf8(roots.stdout).unwrap().contains(&batch));
}

#[test]
#[ignore = "requires a real writable Nix store; owned by prek.verification-root-retention"]
fn real_nix_roots_rotate_and_failed_registration_preserves_the_current_set() {
    let temporary = tempfile::tempdir().unwrap();
    let cache = temporary.path().join("cache");
    let manifest = temporary.path().join("manifest");
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    fs::write(&first, format!("first {}", temporary.path().display())).unwrap();
    fs::write(&second, format!("second {}", temporary.path().display())).unwrap();
    let first = add(&first);
    let second = add(&second);
    success(Command::new(TOOL).arg("init").arg(&cache));
    fs::write(&manifest, format!("{first}\n")).unwrap();
    success(Command::new(TOOL).arg("replace").arg(&cache).arg(&manifest));
    let original = fs::read_link(cache.join("current")).unwrap();
    let root = cache.join(&original).join("batch-0");
    let roots = success(Command::new("nix-store").args(["--query", "--roots", &first]));
    assert!(
        String::from_utf8(roots.stdout)
            .unwrap()
            .contains(root.to_str().unwrap())
    );
    fs::write(
        &manifest,
        "/nix/store/00000000000000000000000000000000-does-not-exist\n",
    )
    .unwrap();
    assert!(
        !Command::new(TOOL)
            .arg("replace")
            .arg(&cache)
            .arg(&manifest)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(fs::read_link(cache.join("current")).unwrap(), original);
    assert_eq!(fs::read_link(&root).unwrap(), Path::new(&first));
    fs::write(&manifest, format!("{second}\n")).unwrap();
    success(Command::new(TOOL).arg("replace").arg(&cache).arg(&manifest));
    assert!(!root.exists());
    let current = fs::read_link(cache.join("current")).unwrap();
    let new_root = cache.join(current).join("batch-0");
    assert_eq!(fs::read_link(&new_root).unwrap(), Path::new(&second));
    let roots = success(Command::new("nix-store").args(["--query", "--roots", &second]));
    assert!(
        String::from_utf8(roots.stdout)
            .unwrap()
            .contains(new_root.to_str().unwrap())
    );
    fs::write(&manifest, format!("{first}\n{second}\n")).unwrap();
    success(Command::new(TOOL).arg("replace").arg(&cache).arg(&manifest));
    let paired = cache.join(fs::read_link(cache.join("current")).unwrap());
    let mut actual = vec![
        fs::read_link(paired.join("batch-0")).unwrap(),
        fs::read_link(paired.join("batch-0-2")).unwrap(),
    ];
    actual.sort();
    let mut expected = vec![
        std::path::PathBuf::from(&first),
        std::path::PathBuf::from(&second),
    ];
    expected.sort();
    assert_eq!(actual, expected);
    fs::write(&manifest, "").unwrap();
    success(Command::new(TOOL).arg("replace").arg(&cache).arg(&manifest));
    assert!(!new_root.exists());
    success(
        Command::new("nix-store")
            .arg("--check-validity")
            .args([first, second]),
    );
}
