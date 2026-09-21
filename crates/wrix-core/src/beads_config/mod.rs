use std::{
    fs, io,
    path::{Path, PathBuf},
};

use displaydoc::Display;
use serde::Deserialize;
use thiserror::Error;

use crate::git::Branch;

#[derive(Debug, Display, Error)]
pub enum ReadError {
    /// cannot read beads configuration at {path}: {source}
    Io { path: PathBuf, source: io::Error },
    /// invalid beads configuration at {path}: {source}
    Yaml {
        path: PathBuf,
        source: serde_yaml_ng::Error,
    },
}

#[derive(Default, Deserialize)]
struct Config {
    #[serde(default, rename = "sync-branch")]
    sync_branch: Branch,
}

pub fn read_sync_branch(workspace: &Path) -> Result<Branch, ReadError> {
    let path = workspace.join(".beads/config.yaml");
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Branch::default()),
        Err(source) => return Err(ReadError::Io { path, source }),
    };
    parse_sync_branch(&content).map_err(|source| ReadError::Yaml { path, source })
}

fn parse_sync_branch(content: &str) -> Result<Branch, serde_yaml_ng::Error> {
    let config: Option<Config> = serde_yaml_ng::from_str(content)?;
    Ok(config.unwrap_or_default().sync_branch)
}

#[cfg(test)]
mod test {
    use super::parse_sync_branch;

    #[test]
    fn sync_branch_parses_yaml_quotes_comments_flow_and_aliases() {
        for content in [
            "sync-branch: team/beads # comment\n",
            "sync-branch: 'team/beads'\n",
            "sync-branch: \"team/beads\"\n",
            "{sync-branch: team/beads, unrelated: true}\n",
            "branch: &branch team/beads\nsync-branch: *branch\n",
        ] {
            assert_eq!(parse_sync_branch(content).unwrap().as_str(), "team/beads");
        }
    }

    #[test]
    fn absent_branch_defaults_without_reading_nested_fields() {
        for content in [
            "",
            "# empty\n",
            "issue-prefix: wx\n",
            "other:\n  sync-branch: unrelated\n",
        ] {
            assert_eq!(
                parse_sync_branch(content).unwrap(),
                crate::git::Branch::default()
            );
        }
    }

    #[test]
    fn malformed_yaml_branch_types_and_unsafe_paths_are_rejected() {
        for content in [
            "sync-branch: /outside",
            "sync-branch: ../outside",
            "sync-branch: team//beads",
            "sync-branch: []",
            "sync-branch: true",
            "sync-branch: 123",
            "sync-branch: null",
            "sync-branch: ''",
            "sync-branch: [",
            "[sync-branch, beads]",
            "sync-branch: one\nsync-branch: two\n",
        ] {
            assert!(parse_sync_branch(content).is_err(), "accepted {content:?}");
        }
        assert_eq!(
            parse_sync_branch("sync-branch: '123'").unwrap().as_str(),
            "123"
        );
    }
}
