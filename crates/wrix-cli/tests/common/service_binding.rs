use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::json;

use super::{TestResult, set_mode};

pub fn provision_celler_verifier(root: &Path) -> TestResult {
    let layout = wrix_cache::backend::layout::Layout::resolve(
        if cfg!(target_os = "macos") {
            wrix_cache::backend::layout::Platform::Darwin
        } else {
            wrix_cache::backend::layout::Platform::Linux
        },
        &root.join("home"),
        Some(&root.join("state")),
        Some(&root.join("cache")),
    )?;
    layout.prepare()?;
    fs::write(
        layout.metadata().join("jwt.pub"),
        include_str!("../fixtures/celler-verifier.pem"),
    )?;
    Ok(())
}

pub fn create(root: &Path, bin: &Path, kind: &str) -> TestResult<PathBuf> {
    fs::create_dir_all(bin)?;
    let source = root.join("bound-source");
    let digest = root.join("bound-digest");
    let layout = root.join("bound-layout");
    fs::create_dir_all(&layout)?;
    fs::write(
        &source,
        serde_json::to_vec(&json!({
            "schema": 1, "source_kind": kind,
            "digest": format!("sha256:{}", "a".repeat(64)),
            "oci_layout": layout, "oci_ref": "latest", "layers": []
        }))?,
    )?;
    fs::write(&digest, format!("sha256:{}\n", "a".repeat(64)))?;
    write(root, bin, kind, &source, &digest)
}

pub fn write(
    root: &Path,
    bin: &Path,
    kind: &str,
    source: &Path,
    digest: &Path,
) -> TestResult<PathBuf> {
    let source_drv = root.join("source.drv");
    let digest_drv = root.join("digest.drv");
    fs::write(&source_drv, source.as_os_str().as_encoded_bytes())?;
    fs::write(&digest_drv, digest.as_os_str().as_encoded_bytes())?;
    let binding = root.join("binding.json");
    fs::write(
        &binding,
        serde_json::to_vec(&json!({
            "schema": 1, "image": "wrix-service:test", "source_kind": kind,
            "source": {"path": source, "derivation": source_drv},
            "digest": {"path": digest, "derivation": digest_drv}
        }))?,
    )?;
    let nix_store = bin.join("nix-store");
    fs::write(
        &nix_store,
        r#"#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  --query) [[ "$2" == --outputs ]]; cat "$3"; printf '\n' ;;
  --realise) [[ "$2" == *.drv && "$3" == --add-root && "$5" == --indirect ]]; output="$(cat "$2")"; [[ -e "$output" ]]; ln -sfn "$output" "$4" ;;
  *) exit 64 ;;
esac
"#,
    )?;
    set_mode(&nix_store, 0o755)?;
    Ok(binding)
}

#[test]
fn fixture_derivation_queries_and_roots_match_bound_artifacts() -> TestResult {
    use wrix_service::lifecycle::binding::Candidate;
    let root = tempfile::tempdir()?;
    let bin = root.path().join("bin");
    let binding = create(root.path(), &bin, "nix-descriptor")?;
    let candidate = Candidate::load(&binding)?;
    let outputs = std::process::Command::new(bin.join("nix-store"))
        .args(["--query", "--outputs"])
        .arg(root.path().join("source.drv"))
        .output()?;
    assert!(outputs.status.success());
    assert_eq!(
        String::from_utf8(outputs.stdout)?.trim(),
        candidate.source().path().to_str().unwrap()
    );
    let link = root.path().join("root");
    assert!(
        std::process::Command::new(bin.join("nix-store"))
            .arg("--realise")
            .arg(root.path().join("source.drv"))
            .arg("--add-root")
            .arg(&link)
            .arg("--indirect")
            .status()?
            .success()
    );
    assert_eq!(fs::read_link(link)?, candidate.source().path());
    Ok(())
}
