//! Audit the actual verifier registry, runner configuration and source selectors.

use std::{collections::BTreeSet, fs, path::Path, process::Command};

const TEST_RUNNER: &str = "bash tests/loom-nextest-file-targets.sh '{paths}'";
const CAPABILITIES: [&str; 3] = ["container-runtime", "kvm", "user-network-namespace"];
const SHARED_FIELDS: [(&str, &str); 6] = [
    ("target", "{capture_1}"),
    ("join", " "),
    ("parse", "json-lines"),
    ("cwd", "."),
    ("skip_policy", "sandbox-capability"),
    ("match", r"^verify:(.+)$"),
];
const DUPLICATED: [&str; 3] = [
    "verify:cli.package-surface",
    "verify:cli.shared-verifier-app",
    "verify:cli.verify-runner-batching",
];
const SELECTORS: &str = "../crates/example/tests/sample.rs::file_test | ../crates/example/src/config/mod.rs::nested_source_test | ../crates/example/src/config.rs::flat_source_test | crates/example/src/lib.rs::test::source_test | module::unit_test";
const FILTER: &str = "test(file_test) + test(nested_source_test) + test(flat_source_test) + test(test::source_test) + test(module::unit_test)";

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// verifier runner policy audit failed: {0}
    Audit(String),
    /// verifier runner policy I/O failed: {0}
    Io(#[from] std::io::Error),
    /// invalid verifier runner TOML: {0}
    Toml(#[from] toml::de::Error),
}

fn runner_errors(entry: &toml::Value, test_ci: bool) -> Vec<String> {
    let mut expected = SHARED_FIELDS.to_vec();
    if test_ci {
        expected.retain(|(key, _)| *key != "match");
        expected.extend([
            ("match", r"^test-ci:(.+)$"),
            ("command", "bin/test-ci-verifiers {targets}"),
            ("inputs", "bin/test-ci-verifiers {print_inputs} {targets}"),
        ]);
    } else {
        expected.extend([
            ("command", "nix run .#verify -- {targets}"),
            ("inputs", "nix run .#verify -- {print_inputs} {targets}"),
        ]);
    }
    let mut errors = Vec::new();
    for (key, expected) in expected {
        if entry.get(key).and_then(toml::Value::as_str) != Some(expected) {
            errors.push(format!(
                "{key} is {:?}, expected {expected:?}",
                entry.get(key)
            ));
        }
    }
    let mut capabilities: Vec<_> = CAPABILITIES
        .iter()
        .map(|name| toml::Value::String((*name).to_owned()))
        .collect();
    if test_ci {
        capabilities.push(toml::Value::String("virtiofsd-capabilities".to_owned()));
    }
    if entry
        .get("skip_capabilities")
        .and_then(toml::Value::as_array)
        != Some(&capabilities)
    {
        errors.push(format!(
            "skip_capabilities is {:?}, expected {capabilities:?}",
            entry.get("skip_capabilities")
        ));
    }
    errors
}

#[derive(Debug, Eq, PartialEq)]
struct Annotation<'a> {
    target: &'a str,
    pending: bool,
}

fn annotations(text: &str, source: bool) -> Vec<Annotation<'_>> {
    let tags: &[(&str, bool)] = if source {
        &[
            ("[test](", false),
            ("[test?](", true),
            ("[judge](", false),
            ("[judge?](", true),
        ]
    } else {
        &[("[check](", false), ("[system](", false)]
    };
    let mut result = Vec::new();
    let mut consumed = 0;
    for (offset, _) in text.match_indices('[') {
        if offset < consumed {
            continue;
        }
        for &(tag, pending) in tags {
            if let Some(rest) = text[offset..].strip_prefix(tag)
                && let Some(end) = rest.find(')')
                && end > 0
                && (source || rest[..end].starts_with("verify:"))
            {
                result.push(Annotation {
                    target: &rest[..end],
                    pending,
                });
                consumed = offset + tag.len() + end + 1;
                break;
            }
        }
    }
    result
}

fn file_selector_error(root: &Path, spec: &Path, target: &str, pending: bool) -> Option<String> {
    let end = target
        .find('#')
        .into_iter()
        .chain(target.find("::"))
        .min()
        .unwrap_or(target.len());
    let path = &target[..end];
    if !path.contains('/') {
        return None;
    }
    let relative = spec.strip_prefix(root).map_or_else(
        |_| spec.display().to_string(),
        |path| path.display().to_string(),
    );
    if path.starts_with("crates/") {
        return Some(format!(
            "{relative} target {target:?} is workspace-relative; file targets must be spec-relative"
        ));
    }
    let Some(parent) = spec.parent() else {
        return Some(format!("specification has no parent: {}", spec.display()));
    };
    let resolved = parent.join(path);
    if !pending && !resolved.is_file() {
        return Some(format!(
            "{relative} target {target:?} resolves to missing file {}",
            resolved.display()
        ));
    }
    None
}

fn logical_annotation_error(
    spec: &Path,
    target: &str,
    inventory: &BTreeSet<String>,
) -> Option<String> {
    if inventory.contains(target) {
        None
    } else {
        Some(format!(
            "{} annotation target {target:?} is absent from .#verify --list",
            spec.display()
        ))
    }
}

fn check_source_annotations(root: &Path, inventory: &BTreeSet<String>) -> Result<(), Error> {
    let mut specs = fs::read_dir(root.join("specs"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    specs.sort();
    for spec in specs {
        if spec.extension().is_none_or(|extension| extension != "md") {
            continue;
        }
        let text = fs::read_to_string(&spec)?;
        for annotation in annotations(&text, true) {
            if let Some(error) =
                file_selector_error(root, &spec, annotation.target, annotation.pending)
            {
                return Err(Error::Audit(error));
            }
        }
        for annotation in annotations(&text, false) {
            if let Some(error) = logical_annotation_error(&spec, annotation.target, inventory) {
                return Err(Error::Audit(error));
            }
        }
    }
    Ok(())
}

fn check_adapter(root: &Path) -> Result<(), Error> {
    let output = Command::new(root.join("tests/loom-nextest-file-targets.sh"))
        .args(["--print-filter", SELECTORS])
        .env_remove("BASH_ENV")
        .env_remove("ENV")
        .output()?;
    if !output.status.success() {
        return Err(Error::Audit(format!(
            "file selector adapter failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let actual = String::from_utf8_lossy(&output.stdout);
    if actual.trim() != FILTER {
        return Err(Error::Audit(format!(
            "file selector adapter emitted unexpected filter {actual:?}"
        )));
    }
    Ok(())
}

pub fn audit(config: &Path, inventory_text: &str) -> Result<(), Error> {
    let inventory: BTreeSet<_> = inventory_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    if inventory.is_empty() {
        return Err(Error::Audit(
            ".#verify --list inventory is empty".to_owned(),
        ));
    }
    let Some(root) = config.parent() else {
        return Err(Error::Audit(
            "runner configuration has no repository parent".to_owned(),
        ));
    };
    let raw = fs::read_to_string(config)?;
    let document: toml::Value = toml::from_str(&raw)?;
    let runner = document
        .get("runner")
        .ok_or_else(|| Error::Audit("missing [runner]".to_owned()))?;
    for tier in ["check", "system"] {
        for name in ["verify", "test-ci"] {
            let entry = runner
                .get(tier)
                .and_then(|tier| tier.get(name))
                .ok_or_else(|| Error::Audit(format!("missing [runner.{tier}.{name}]")))?;
            let errors = runner_errors(entry, name == "test-ci");
            if !errors.is_empty() {
                return Err(Error::Audit(format!(
                    "[runner.{tier}.{name}] {}",
                    errors.join("; ")
                )));
            }
        }
    }
    if runner
        .get("test")
        .and_then(|test| test.get("command"))
        .and_then(toml::Value::as_str)
        != Some(TEST_RUNNER)
    {
        return Err(Error::Audit(format!(
            "[runner.test] command must be {TEST_RUNNER:?}"
        )));
    }
    check_source_annotations(root, &inventory)?;
    check_adapter(root)?;
    for duplicate in DUPLICATED {
        if raw.contains(duplicate) {
            return Err(Error::Audit(format!(
                "loom.toml duplicates verifier target {duplicate}; use .#verify --list as the registry"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod test;
