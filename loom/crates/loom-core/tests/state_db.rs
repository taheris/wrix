//! Integration tests for `loom_core::state::StateDb`.
//!
//! Each test name maps 1:1 onto a shell-level acceptance test in
//! `tests/loom-test.sh::test_state_*`. The shell harness invokes these via
//! `cargo test -p loom-core --test state_db <name>` so the verify path runs
//! the real implementation, not a stub.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use loom_core::identifier::{MoleculeId, SpecLabel};
use loom_core::state::{ActiveMolecule, StateDb};

fn write_spec(workspace: &Path, label: &str, body: &str) -> Result<()> {
    let specs = workspace.join("specs");
    std::fs::create_dir_all(&specs)?;
    std::fs::write(specs.join(format!("{label}.md")), body)?;
    Ok(())
}

fn list_table(db_path: &Path, sql: &str) -> Result<Vec<Vec<String>>> {
    let conn = rusqlite::Connection::open(db_path)?;
    let mut stmt = conn.prepare(sql)?;
    let cols = stmt.column_count();
    let rows: Vec<Vec<String>> = stmt
        .query_map([], |row| {
            (0..cols)
                .map(|i| {
                    let v: rusqlite::types::Value = row.get(i)?;
                    Ok(match v {
                        rusqlite::types::Value::Null => String::from("NULL"),
                        rusqlite::types::Value::Integer(i) => i.to_string(),
                        rusqlite::types::Value::Real(r) => r.to_string(),
                        rusqlite::types::Value::Text(t) => t,
                        rusqlite::types::Value::Blob(_) => String::from("<blob>"),
                    })
                })
                .collect::<rusqlite::Result<Vec<String>>>()
        })?
        .collect::<rusqlite::Result<Vec<Vec<String>>>>()?;
    Ok(rows)
}

#[test]
fn state_db_init_creates_tables() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("state.db");
    let _db = StateDb::open(&db_path)?;
    assert!(db_path.exists(), "state.db should be created");

    let tables = list_table(
        &db_path,
        "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
    )?;
    let names: Vec<&str> = tables.iter().map(|r| r[0].as_str()).collect();
    for expected in ["companions", "meta", "molecules", "specs"] {
        assert!(
            names.contains(&expected),
            "expected table {expected}: {names:?}"
        );
    }

    let meta = list_table(
        &db_path,
        "SELECT key, value FROM meta WHERE key='schema_version'",
    )?;
    assert_eq!(
        meta,
        vec![vec!["schema_version".to_string(), "1".to_string()]]
    );
    Ok(())
}

#[test]
fn state_db_rebuild_populates_specs_and_molecules() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    write_spec(workspace, "alpha", "# alpha\n\nbody\n")?;
    write_spec(workspace, "beta", "# beta\n\nbody\n")?;

    let db = StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let molecules = vec![ActiveMolecule {
        id: MoleculeId::new("wx-alpha"),
        spec_label: SpecLabel::new("alpha"),
        base_commit: Some("abc123".to_string()),
    }];

    let report = db.rebuild(workspace, &molecules)?;
    assert_eq!(report.specs, 2);
    assert_eq!(report.molecules, 1);

    let alpha = db.spec(&SpecLabel::new("alpha"))?;
    assert_eq!(alpha.spec_path.to_str(), Some("specs/alpha.md"));
    assert!(alpha.implementation_notes.is_none());

    let mol = db
        .active_molecule(&SpecLabel::new("alpha"))?
        .context("molecule should be present after rebuild")?;
    assert_eq!(mol.id.as_str(), "wx-alpha");
    assert_eq!(mol.base_commit.as_deref(), Some("abc123"));
    assert_eq!(mol.iteration_count, 0);

    assert!(db.active_molecule(&SpecLabel::new("beta"))?.is_none());
    Ok(())
}

#[test]
fn state_db_rebuild_companions() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    write_spec(
        workspace,
        "with-companions",
        "# spec\n\n## Companions\n\n- `lib/a/`\n- `lib/b/`\n\n## Other\n\n- `lib/skip/`\n",
    )?;
    write_spec(
        workspace,
        "no-companions",
        "# bare spec\n\nno section here\n",
    )?;

    let db = StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let report = db.rebuild(workspace, &[])?;
    assert_eq!(report.specs, 2);
    assert_eq!(report.companions, 2);

    let rows = list_table(
        &workspace.join(".wrapix/loom/state.db"),
        "SELECT spec_label, companion_path FROM companions ORDER BY spec_label, companion_path",
    )?;
    assert_eq!(
        rows,
        vec![
            vec!["with-companions".to_string(), "lib/a/".to_string()],
            vec!["with-companions".to_string(), "lib/b/".to_string()],
        ]
    );
    Ok(())
}

#[test]
fn state_db_rebuild_resets_counters() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    write_spec(workspace, "alpha", "# alpha\n")?;
    let db = StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let molecules = vec![ActiveMolecule {
        id: MoleculeId::new("wx-alpha"),
        spec_label: SpecLabel::new("alpha"),
        base_commit: None,
    }];
    db.rebuild(workspace, &molecules)?;

    let mol_id = MoleculeId::new("wx-alpha");
    assert_eq!(db.increment_iteration(&mol_id)?, 1);
    assert_eq!(db.increment_iteration(&mol_id)?, 2);

    db.rebuild(workspace, &molecules)?;
    let mol = db
        .active_molecule(&SpecLabel::new("alpha"))?
        .context("molecule still present after rebuild")?;
    assert_eq!(mol.iteration_count, 0);
    Ok(())
}

#[test]
fn state_current_spec_round_trips() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = StateDb::open(dir.path().join("state.db"))?;
    assert!(db.current_spec()?.is_none());

    let label = SpecLabel::new("loom-harness");
    db.set_current_spec(&label)?;
    assert_eq!(db.current_spec()?, Some(label.clone()));

    let other = SpecLabel::new("ralph-loop");
    db.set_current_spec(&other)?;
    assert_eq!(db.current_spec()?, Some(other));
    Ok(())
}

#[test]
fn state_increment_iteration_returns_updated_count() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    write_spec(workspace, "alpha", "# alpha\n")?;
    let db = StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let molecules = vec![ActiveMolecule {
        id: MoleculeId::new("wx-alpha"),
        spec_label: SpecLabel::new("alpha"),
        base_commit: None,
    }];
    db.rebuild(workspace, &molecules)?;

    let mol_id = MoleculeId::new("wx-alpha");
    assert_eq!(db.increment_iteration(&mol_id)?, 1);
    assert_eq!(db.increment_iteration(&mol_id)?, 2);
    assert_eq!(db.increment_iteration(&mol_id)?, 3);
    Ok(())
}

#[test]
fn state_corruption_recovery() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("state.db");
    std::fs::write(&db_path, b"this is not a sqlite database\x00\x01\x02")?;

    if StateDb::open(&db_path).is_ok() {
        return Err(anyhow!("opening a corrupt db should fail"));
    }

    let db = StateDb::recreate(&db_path)?;
    let workspace = dir.path();
    write_spec(workspace, "alpha", "# alpha\n")?;
    let report = db.rebuild(workspace, &[])?;
    assert_eq!(report.specs, 1);
    Ok(())
}
