use super::*;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn fixture(metadata: &str, databases: &[&str]) -> TestResult<tempfile::TempDir> {
    let workspace = tempfile::tempdir()?;
    let root = workspace.path().join(".beads/dolt");
    fs::create_dir_all(&root)?;
    fs::write(workspace.path().join(".beads/metadata.json"), metadata)?;
    for database in databases {
        fs::create_dir_all(root.join(database).join(".dolt/noms"))?;
    }
    Ok(workspace)
}

#[test]
fn configured_database_wins_over_server_root_graph_and_nested_statistics() -> TestResult {
    let workspace = fixture(
        r#"{"dolt_database":"lm","dolt_mode":"server"}"#,
        &["", "lm", "graph", ".dolt/stats", "lm/.dolt/stats"],
    )?;
    assert_eq!(
        locate(workspace.path())?,
        workspace.path().join(".beads/dolt/lm")
    );
    assert!(
        workspace
            .path()
            .join(".beads/dolt/graph/.dolt/noms")
            .is_dir()
    );
    Ok(())
}

#[test]
fn missing_configured_database_does_not_import_an_unrelated_sibling() -> TestResult {
    let workspace = fixture(r#"{"dolt_database":"lm"}"#, &["", "graph"])?;
    assert!(matches!(
        locate(workspace.path()),
        Err(Error::SourceAmbiguous { .. })
    ));
    Ok(())
}

#[test]
fn server_root_and_sibling_are_ambiguous_without_project_identity() -> TestResult {
    let workspace = fixture("{}", &["", "graph"])?;
    assert!(matches!(
        locate(workspace.path()),
        Err(Error::SourceAmbiguous { .. })
    ));
    Ok(())
}

#[test]
fn flat_legacy_database_is_supported_without_sibling_databases() -> TestResult {
    let workspace = fixture(r#"{"dolt_database":"lm"}"#, &[""])?;
    assert_eq!(
        locate(workspace.path())?,
        workspace.path().join(".beads/dolt")
    );
    Ok(())
}

#[test]
fn configured_source_cannot_traverse_out_of_the_admitted_data_root() -> TestResult {
    for name in ["../peer", "/peer", "lm/../graph", ".", "", "lm/"] {
        let workspace = fixture(
            &serde_json::json!({"dolt_database": name}).to_string(),
            &["lm"],
        )?;
        assert!(matches!(locate(workspace.path()), Err(Error::UnsafePath)));
    }
    Ok(())
}
