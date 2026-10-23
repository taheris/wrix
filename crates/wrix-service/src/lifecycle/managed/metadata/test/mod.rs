use super::*;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const DATABASE: &str = "private_namespace";

fn fixture() -> TestResult<tempfile::TempDir> {
    let root = tempfile::tempdir()?;
    let output = Command::new("git")
        .args(["-c", "init.templateDir=", "init", "-q"])
        .arg(root.path())
        .output()?;
    assert!(output.status.success());
    fs::create_dir(root.path().join(".beads"))?;
    Ok(root)
}

fn stage(root: &Path, bytes: &[u8]) -> TestResult {
    fs::write(root.join(".beads/metadata.json"), bytes)?;
    let output = Command::new("git")
        .args(["add", "--", ".beads/metadata.json"])
        .current_dir(root)
        .output()?;
    assert!(output.status.success());
    Ok(())
}

fn projection(original: &[u8]) -> TestResult<Vec<u8>> {
    let mut value: serde_json::Value = serde_json::from_slice(original)?;
    value["dolt_database"] = DATABASE.into();
    value["dolt_mode"] = "server".into();
    Ok(serde_json::to_vec_pretty(&value)?)
}

#[test]
fn legacy_runtime_projection_restores_exact_indexed_bytes_and_newline() -> TestResult {
    let root = fixture()?;
    let original = b"{ \"backend\": \"dolt\", \"dolt_database\": \"beads\", \"dolt_mode\": \"embedded\", \"project_id\": \"staged-user-choice\" }\n";
    stage(root.path(), original)?;
    fs::write(
        root.path().join(".beads/metadata.json"),
        projection(original)?,
    )?;
    restore(root.path(), DATABASE)?;
    restore(root.path(), DATABASE)?;
    assert_eq!(
        fs::read(root.path().join(".beads/metadata.json"))?,
        original
    );
    let indexed = Command::new("git")
        .args(["show", ":.beads/metadata.json"])
        .current_dir(root.path())
        .output()?;
    assert!(indexed.status.success());
    assert_eq!(indexed.stdout, original);
    let diff = Command::new("git")
        .args(["diff", "--exit-code", "--", ".beads/metadata.json"])
        .current_dir(root.path())
        .output()?;
    assert!(diff.status.success());
    Ok(())
}

#[test]
fn legacy_cleanup_preserves_additional_user_edits_byte_for_byte() -> TestResult {
    let root = fixture()?;
    let original = br#"{"backend":"dolt","dolt_database":"beads"}"#;
    stage(root.path(), original)?;
    let mut user: serde_json::Value = serde_json::from_slice(&projection(original)?)?;
    user["project_id"] = "user-edited".into();
    let bytes = serde_json::to_vec_pretty(&user)?;
    fs::write(root.path().join(".beads/metadata.json"), &bytes)?;
    restore(root.path(), DATABASE)?;
    assert_eq!(fs::read(root.path().join(".beads/metadata.json"))?, bytes);
    Ok(())
}

#[test]
fn legacy_cleanup_does_not_replace_untracked_metadata() -> TestResult {
    let root = fixture()?;
    let bytes = projection(br#"{"backend":"dolt","dolt_database":"beads"}"#)?;
    fs::write(root.path().join(".beads/metadata.json"), &bytes)?;
    restore(root.path(), DATABASE)?;
    assert_eq!(fs::read(root.path().join(".beads/metadata.json"))?, bytes);
    Ok(())
}
