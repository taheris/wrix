use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Write},
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::{Value, json};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
type Derivations = BTreeMap<String, String>;

fn execute(command: &mut Command) -> Result<Output> {
    let output = command.output()?;
    if !output.status.success() {
        return Err(format!(
            "command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(output)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_dir() {
        fs::create_dir(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
        }
    } else if metadata.file_type().is_symlink() {
        symlink(fs::read_link(source)?, destination)?;
    } else {
        fs::copy(source, destination)?;
    }
    Ok(())
}

fn append(path: &Path, text: &str) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(permissions.mode() | 0o200);
    fs::set_permissions(path, permissions)?;
    let mut file = fs::OpenOptions::new().append(true).open(path)?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

fn nix(root: &Path, git_env: &[String], args: &[&str], source: Option<&Path>) -> Result<Value> {
    let mut command = Command::new("nix");
    command.current_dir(root).args(args);
    for name in git_env {
        command.env_remove(name);
    }
    if let Some(source) = source {
        command.env("WRIX_SOURCE_PROBE", source);
    }
    Ok(serde_json::from_slice(&execute(&mut command)?.stdout)?)
}

fn derivations(root: &Path, git_env: &[String], source: &Path) -> Result<Derivations> {
    let value = nix(
        root,
        git_env,
        &[
            "eval",
            "--impure",
            "--json",
            "--expr",
            r#"
      let
        source = builtins.getEnv "WRIX_SOURCE_PROBE";
        flake = builtins.getFlake ("path:" + source);
        system = builtins.currentSystem;
        pkgs = import flake.inputs.nixpkgs { inherit system; config.allowUnfree = true; };
        rust = import (source + "/lib/services/rust.nix") {
          inherit pkgs;
          rustProfile = flake.legacyPackages.${system}.lib.profiles.rust;
        };
      in {
        launcher = flake.packages.${system}.wrix.drvPath;
        image = flake.packages.${system}.image-rust.drvPath;
        clippy = flake.legacyPackages.${system}.ciChecks.wrix-rust-clippy.drvPath;
        nextest = flake.legacyPackages.${system}.ciChecks.wrix-rust-nextest.drvPath;
        dependencies = rust.cargoArtifacts.drvPath;
      }
    "#,
        ],
        Some(source),
    )?;
    Ok(serde_json::from_value(value)?)
}

fn compare(before: &Derivations, after: &Derivations, production: bool) -> Result<()> {
    for key in ["launcher", "image", "clippy", "nextest", "dependencies"] {
        let left = before.get(key).ok_or_else(|| format!("missing {key}"))?;
        let right = after.get(key).ok_or_else(|| format!("missing {key}"))?;
        let changed = production && key != "dependencies" || matches!(key, "clippy" | "nextest");
        if (left != right) != changed {
            return Err(format!("{key}: expected changed={changed}; {left} -> {right}").into());
        }
    }
    Ok(())
}

fn run(root: &Path) -> Result<Value> {
    let root = root.canonicalize()?;
    let git_env = String::from_utf8(
        execute(
            Command::new("git")
                .current_dir(&root)
                .args(["rev-parse", "--local-env-vars"]),
        )?
        .stdout,
    )?;
    let git_env: Vec<_> = git_env.lines().map(str::to_owned).collect();
    let metadata = nix(
        &root,
        &git_env,
        &[
            "flake",
            "metadata",
            "--json",
            "--no-write-lock-file",
            &format!("git+file://{}", root.display()),
        ],
        None,
    )?;
    let snapshot = metadata["path"].as_str().ok_or("missing source snapshot")?;
    let temporary = tempfile::tempdir()?;
    let source = temporary.path().join("source");
    copy_tree(Path::new(snapshot), &source)?;
    let original = derivations(&root, &git_env, &source)?;
    append(
        &source.join(".config/nextest.toml"),
        "\n# Source-isolation probe.\n",
    )?;
    let configuration = derivations(&root, &git_env, &source)?;
    compare(&original, &configuration, false)?;
    append(
        &source.join("crates/wrix-cli/tests/arguments.rs"),
        "\n// Source-isolation probe.\n",
    )?;
    let integration = derivations(&root, &git_env, &source)?;
    compare(&configuration, &integration, false)?;
    append(
        &source.join("crates/wrix-cli/src/main.rs"),
        "\n// Source-isolation probe.\n",
    )?;
    let production = derivations(&root, &git_env, &source)?;
    compare(&integration, &production, true)?;
    Ok(
        json!({"original": original, "configuration_edit": configuration, "integration_edit": integration, "production_edit": production}),
    )
}

fn main() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let root = PathBuf::from(
        args.next()
            .ok_or("usage: repository-source-isolation ROOT")?,
    );
    if args.next().is_some() {
        return Err("usage: repository-source-isolation ROOT".into());
    }
    let report = run(&root)?;
    serde_json::to_writer(io::stdout().lock(), &report)?;
    io::stdout().lock().write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> Derivations {
        ["launcher", "image", "clippy", "nextest", "dependencies"]
            .into_iter()
            .map(|key| (key.into(), key.into()))
            .collect()
    }

    #[test]
    fn detector_rejects_each_wrong_dependency_or_missing_invalidation() {
        for production in [false, true] {
            let before = values();
            let mut after = before.clone();
            for key in ["launcher", "image", "clippy", "nextest", "dependencies"] {
                if production && key != "dependencies" || matches!(key, "clippy" | "nextest") {
                    after.insert(key.into(), format!("changed-{key}"));
                }
            }
            assert!(compare(&before, &after, production).is_ok());
            for key in before.keys() {
                let mut broken = after.clone();
                broken.insert(
                    key.clone(),
                    if broken[key] == before[key] {
                        "unexpected-change".into()
                    } else {
                        before[key].clone()
                    },
                );
                assert!(
                    compare(&before, &broken, production).is_err(),
                    "{production}: {key}"
                );
                broken.remove(key);
                assert!(compare(&before, &broken, production).is_err());
            }
        }
    }

    #[test]
    fn snapshots_preserve_links_and_executable_bits_without_mutating_original() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("original");
        fs::create_dir(&original).unwrap();
        fs::write(original.join("program"), "data").unwrap();
        fs::set_permissions(original.join("program"), fs::Permissions::from_mode(0o555)).unwrap();
        symlink("program", original.join("link")).unwrap();
        let copy = root.path().join("copy");
        copy_tree(&original, &copy).unwrap();
        assert_eq!(
            fs::read_link(copy.join("link")).unwrap(),
            Path::new("program")
        );
        assert_eq!(
            fs::metadata(copy.join("program"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o555
        );
        append(&copy.join("program"), " appended").unwrap();
        assert_eq!(
            fs::read_to_string(original.join("program")).unwrap(),
            "data"
        );
        assert_eq!(
            fs::read_to_string(copy.join("link")).unwrap(),
            "data appended"
        );
    }
}
