use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Write},
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::Value;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
type Derivations = BTreeMap<String, String>;
const DERIVATIONS: [&str; 6] = [
    "launcher",
    "image",
    "clippy",
    "nextest",
    "dependencies",
    "backend",
];

#[derive(Clone, Copy, Debug)]
enum Change {
    Configuration,
    Integration,
    Production,
}

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

fn snapshot(source: &Path, destination: &Path) -> Result<PathBuf> {
    copy_tree(source, destination)?;
    // Nix path literals cannot traverse symlinked temporary-directory ancestors.
    Ok(destination.canonicalize()?)
}

fn append(path: &Path, text: &str) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(permissions.mode() | 0o200);
    fs::set_permissions(path, permissions)?;
    let mut file = fs::OpenOptions::new().append(true).open(path)?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

fn nix(root: &Path, git_env: &[String], args: &[&str], sources: Option<&str>) -> Result<Value> {
    let mut command = Command::new("nix");
    command.current_dir(root).args(args);
    for name in git_env {
        command.env_remove(name);
    }
    if let Some(sources) = sources {
        command.env("WRIX_SOURCE_PROBES", sources);
    }
    Ok(serde_json::from_slice(&execute(&mut command)?.stdout)?)
}

fn derivations(
    root: &Path,
    git_env: &[String],
    sources: &BTreeMap<&str, PathBuf>,
) -> Result<BTreeMap<String, Derivations>> {
    let sources = serde_json::to_string(sources)?;
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
        sources = builtins.fromJSON (builtins.getEnv "WRIX_SOURCE_PROBES");
        reference = builtins.getFlake ("path:" + sources.original);
        system = builtins.currentSystem;
        pkgs = import reference.inputs.nixpkgs { inherit system; config.allowUnfree = true; };
        derive = source: let
        flake = builtins.getFlake ("path:" + source);
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
        backend = rust.cacheBackendTests.drvPath;
      };
      in builtins.mapAttrs (_: derive) sources
    "#,
        ],
        Some(&sources),
    )?;
    Ok(serde_json::from_value(value)?)
}

fn compare(before: &Derivations, after: &Derivations, change: Change) -> Result<()> {
    for key in DERIVATIONS {
        let left = before.get(key).ok_or_else(|| format!("missing {key}"))?;
        let right = after.get(key).ok_or_else(|| format!("missing {key}"))?;
        let changed = matches!(change, Change::Production) && key != "dependencies"
            || matches!(key, "clippy" | "nextest")
            || key == "backend" && matches!(change, Change::Integration);
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
    let original_source = metadata["path"].as_str().ok_or("missing source snapshot")?;
    let temporary = tempfile::tempdir()?;
    let mut previous = PathBuf::from(original_source);
    let mut sources = BTreeMap::new();
    for (name, edit) in [
        ("original", None),
        (
            "configuration_edit",
            Some((".config/nextest.toml", "\n# Source-isolation probe.\n")),
        ),
        (
            "integration_edit",
            Some((
                "crates/wrix-cli/tests/arguments.rs",
                "\n// Source-isolation probe.\n",
            )),
        ),
        (
            "production_edit",
            Some((
                "crates/wrix-cli/src/main.rs",
                "\n// Source-isolation probe.\n",
            )),
        ),
    ] {
        let parent = temporary.path().join(name);
        fs::create_dir(&parent)?;
        let source = snapshot(&previous, &parent.join("source"))?;
        if let Some((path, text)) = edit {
            append(&source.join(path), text)?;
        }
        sources.insert(name, source.clone());
        previous = source;
    }
    // Freeze every variant before sharing one evaluator; cached reads must never
    // conceal a subsequent source mutation.
    let reports = derivations(&root, &git_env, &sources)?;
    for (before, after, change) in [
        ("original", "configuration_edit", Change::Configuration),
        (
            "configuration_edit",
            "integration_edit",
            Change::Integration,
        ),
        ("integration_edit", "production_edit", Change::Production),
    ] {
        compare(
            reports
                .get(before)
                .ok_or_else(|| format!("missing {before}"))?,
            reports
                .get(after)
                .ok_or_else(|| format!("missing {after}"))?,
            change,
        )?;
    }
    Ok(serde_json::to_value(reports)?)
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
        DERIVATIONS
            .into_iter()
            .map(|key| (key.into(), key.into()))
            .collect()
    }

    #[test]
    fn detector_rejects_each_wrong_dependency_or_missing_invalidation() {
        for (change, changed_keys) in [
            (Change::Configuration, &["clippy", "nextest"][..]),
            (Change::Integration, &["clippy", "nextest", "backend"][..]),
            (
                Change::Production,
                &["launcher", "image", "clippy", "nextest", "backend"][..],
            ),
        ] {
            let before = values();
            let mut after = before.clone();
            for key in changed_keys {
                after.insert((*key).into(), format!("changed-{key}"));
            }
            assert!(compare(&before, &after, change).is_ok());
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
                    compare(&before, &broken, change).is_err(),
                    "{change:?}: {key}"
                );
                broken.remove(key);
                assert!(compare(&before, &broken, change).is_err());
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
        let alias = root.path().join("alias");
        symlink(root.path().canonicalize().unwrap(), &alias).unwrap();
        let copy = snapshot(&original, &alias.join("copy")).unwrap();
        assert_eq!(copy, root.path().canonicalize().unwrap().join("copy"));
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
