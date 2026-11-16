#![cfg(test)]

use std::{
    fs,
    path::Path,
    process::{Command, Output},
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
