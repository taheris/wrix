use super::{csv, snapshot};
use crate::native_fixture::command;
use std::{path::Path, process::Command, time::Duration};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// historical snapshot access failed: {0}
    Snapshot(#[from] snapshot::Error),
    /// historical CSV failed: {0}
    Csv(#[from] csv::Error),
    /// historical count must be a single unsigned integer record
    Count,
    /// repair must retain the issue count plus exactly two explicit new issues
    Issues,
    /// historical commit list must contain ten supported commit hashes
    History,
    /// read-only probe command failed: {0}
    Command(#[from] command::Error),
    /// read-only probe did not complete successfully
    Failed,
    /// retained history count does not equal ten
    Retained,
    /// native inspect resources do not retain four CPUs and one GiB
    Resources,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(transparent)]
pub struct Commit(String);
impl Commit {
    pub fn parse(value: &str) -> Result<Self, Error> {
        let valid = match value.len() {
            32 => value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
            40 | 64 => value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            _ => false,
        };
        if !valid {
            return Err(Error::History);
        }
        Ok(Self(value.to_owned()))
    }
}

pub struct Plan {
    commits: [Commit; 10],
}
fn count(bytes: &[u8], column: &'static str) -> Result<u64, Error> {
    let table = csv::Table::parse(bytes)?;
    let values = table.column(column)?;
    let [value] = values.as_slice() else {
        return Err(Error::Count);
    };
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::Count);
    }
    value.parse().map_err(|_| Error::Count)
}
impl Plan {
    pub fn load(root: &snapshot::Directory) -> Result<Self, Error> {
        let before = root.read("issues-before-native-update.csv")?;
        let after = root.read("issues-after-fixed-repair.csv")?;
        let history = root.read("history-before-native-update.csv")?;
        Self::parse(&before, &after, &history)
    }
    pub fn parse(before: &[u8], after: &[u8], history: &[u8]) -> Result<Self, Error> {
        let expected = count(before, "issue_count")?
            .checked_add(2)
            .ok_or(Error::Issues)?;
        if count(after, "issue_count")? != expected {
            return Err(Error::Issues);
        }
        let history = csv::Table::parse(history)?;
        let commits: Vec<_> = history
            .column("commit_hash")?
            .into_iter()
            .map(Commit::parse)
            .collect::<Result<_, _>>()?;
        let commits = commits.try_into().map_err(|_| Error::History)?;
        Ok(Self { commits })
    }
    pub fn query(&self) -> String {
        let literals: Vec<_> = self
            .commits
            .iter()
            .map(|commit| format!("'{}'", commit.0))
            .collect();
        format!(
            "SELECT COUNT(*) AS retained FROM dolt_log WHERE commit_hash IN ({})",
            literals.join(",")
        )
    }
    pub fn run(&self, bd: &Path, container: &Path) -> Result<Receipt, Error> {
        let result = command::capture(
            Command::new(bd).args(["sql", "--csv", &self.query()]),
            Duration::from_secs(90),
        )?;
        if !result.status.success() {
            return Err(Error::Failed);
        }
        if count(&result.stdout, "retained")? != 10 {
            return Err(Error::Retained);
        }
        let result = command::capture(
            Command::new(container).args(["inspect", "wrix-host-service"]),
            Duration::from_secs(90),
        )?;
        if !result.status.success() {
            return Err(Error::Failed);
        }
        let resources = Resources::parse(&result.stdout)?;
        Ok(Receipt {
            issue_count_preserved_with_two_explicit_new_tasks: true,
            ten_premaintenance_commits_still_reachable: true,
            retained_backend_resources: resources.0,
        })
    }
}

#[derive(serde::Serialize)]
pub struct Receipt {
    issue_count_preserved_with_two_explicit_new_tasks: bool,
    ten_premaintenance_commits_still_reachable: bool,
    retained_backend_resources: serde_json::Map<String, serde_json::Value>,
}
struct Resources(serde_json::Map<String, serde_json::Value>);
impl Resources {
    fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let rows: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| Error::Resources)?;
        let Some(rows) = rows.as_array() else {
            return Err(Error::Resources);
        };
        let [row] = rows.as_slice() else {
            return Err(Error::Resources);
        };
        let resources = row
            .get("configuration")
            .and_then(|v| v.get("resources"))
            .and_then(serde_json::Value::as_object)
            .ok_or(Error::Resources)?;
        if resources.get("cpus").and_then(serde_json::Value::as_u64) != Some(4)
            || resources
                .get("memoryInBytes")
                .and_then(serde_json::Value::as_u64)
                != Some(1_073_741_824)
        {
            return Err(Error::Resources);
        }
        Ok(Self(resources.clone()))
    }
}

#[cfg(test)]
mod test;
