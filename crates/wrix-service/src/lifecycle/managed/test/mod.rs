use std::fs;

use super::configure;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn managed_policy_preserves_sync_identity_and_unrelated_local_settings() -> TestResult {
    let root = tempfile::tempdir()?;
    let beads = root.path().join(".beads");
    fs::create_dir(&beads)?;
    let config = "sync:\n  mode: dolt-native\nsync-branch: custom/beads\n";
    fs::write(beads.join("config.yaml"), config)?;
    fs::write(
        beads.join("metadata.json"),
        r#"{"backend":"dolt","dolt_mode":"embedded","dolt_database":"lm","project_id":"keep"}"#,
    )?;
    fs::write(
        beads.join("config.local.yaml"),
        "custom: keep\ndolt:\n  auto-start: true\n  max-conns: 5\nimport.auto: true\n",
    )?;
    configure(root.path())?;
    let local = fs::read_to_string(beads.join("config.local.yaml"))?;
    let parsed: serde_yaml_ng::Value = serde_yaml_ng::from_str(&local)?;
    assert_eq!(parsed["dolt.auto-start"], false);
    assert_eq!(parsed["dolt"]["max-conns"], 5);
    assert_eq!(parsed["custom"], "keep");
    assert_eq!(parsed["import.auto"], false);
    assert_eq!(parsed["export.auto"], false);
    assert_eq!(fs::read_to_string(beads.join("config.yaml"))?, config);
    let metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(beads.join("metadata.json"))?)?;
    assert_eq!(metadata["dolt_mode"], "server");
    assert_eq!(metadata["dolt_database"], "lm");
    assert_eq!(metadata["project_id"], "keep");
    configure(root.path())?;
    assert_eq!(fs::read_to_string(beads.join("config.local.yaml"))?, local);
    Ok(())
}

#[test]
fn malformed_local_policy_fails_without_overwriting_configuration() -> TestResult {
    let root = tempfile::tempdir()?;
    let beads = root.path().join(".beads");
    fs::create_dir(&beads)?;
    fs::write(beads.join("config.yaml"), "sync.mode: dolt-native\n")?;
    let metadata = r#"{"backend":"dolt","dolt_mode":"embedded"}"#;
    fs::write(beads.join("metadata.json"), metadata)?;
    let invalid = "dolt: [broken\n";
    fs::write(beads.join("config.local.yaml"), invalid)?;
    assert!(configure(root.path()).is_err());
    assert_eq!(
        fs::read_to_string(beads.join("config.local.yaml"))?,
        invalid
    );
    assert_eq!(fs::read_to_string(beads.join("metadata.json"))?, metadata);
    Ok(())
}
