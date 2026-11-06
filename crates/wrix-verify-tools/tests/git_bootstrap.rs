#![cfg(test)]

mod common;

use std::{
    env, fs,
    path::Path,
    process::{Command, Output},
};

fn invoke(program: &str, args: &[&str], home: &Path, directory: &Path) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(directory)
        .env_clear()
        .env("HOME", home)
        .env("PATH", env::var_os("PATH").unwrap())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

#[test]
fn global_bootstrap_works_before_workspace_repository_discovery() {
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("home");
    let workspace = temporary.path().join("workspace");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&workspace).unwrap();
    let marker = workspace.join(".git");
    let original = format!(
        "gitdir: {}\n",
        temporary
            .path()
            .join("unavailable-host-git-directory")
            .display()
    );
    fs::write(&marker, &original).unwrap();
    let script = common::repo_file("lib/util/git-ssh-setup.sh");
    let result = Command::new("bash")
        .args(["-c", "source \"$1\" 0", "bootstrap"])
        .arg(&script)
        .current_dir(&workspace)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", env::var_os("PATH").unwrap())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read_to_string(marker).unwrap(), original);
    for (key, expected) in [
        ("user.name", "Wrix Sandbox"),
        ("user.email", "sandbox@wrix.dev"),
    ] {
        let value = invoke(
            "git",
            &["-C", "/", "config", "--global", "--get", key],
            &home,
            &workspace,
        );
        assert!(
            value.status.success(),
            "{}",
            String::from_utf8_lossy(&value.stderr)
        );
        assert_eq!(String::from_utf8(value.stdout).unwrap().trim(), expected);
    }
    let dependent = invoke(
        "git",
        &["config", "--global", "--get", "user.name"],
        &home,
        &workspace,
    );
    assert!(!dependent.status.success());
    assert!(String::from_utf8_lossy(&dependent.stderr).contains("not a git repository"));
}
