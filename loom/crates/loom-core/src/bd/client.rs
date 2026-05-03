use std::ffi::OsString;
use std::time::Duration;

use serde::Deserialize;

use crate::identifier::{BeadId, MoleculeId};

use super::error::BdError;
use super::models::{Bead, MolProgress};
use super::runner::{CommandRunner, RunOutput, TokioRunner, render_args};

/// Default subprocess timeout. Configurable per [`BdClient`] instance via
/// [`BdClient::with_timeout`]. Matches the 60-second ceiling used by
/// `GitClient`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Typed wrapper around the `bd` CLI.
pub struct BdClient<R: CommandRunner = TokioRunner> {
    runner: R,
    timeout: Duration,
}

impl BdClient<TokioRunner> {
    /// Construct a client that shells out to the real `bd` binary.
    pub fn new() -> Self {
        Self {
            runner: TokioRunner,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl Default for BdClient<TokioRunner> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: CommandRunner> BdClient<R> {
    /// Construct a client over a custom [`CommandRunner`] (used by tests
    /// to substitute a capturing fake).
    pub fn with_runner(runner: R) -> Self {
        Self {
            runner,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Override the per-call subprocess timeout.
    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// `bd show <id> --json` → first row.
    pub async fn show(&self, id: &BeadId) -> Result<Bead, BdError> {
        let args = args(["show", id.as_str(), "--json"]);
        let out = self.invoke(args).await?;
        let mut beads: Vec<Bead> = decode(&out.stdout, &out.args)?;
        if beads.is_empty() {
            return Err(BdError::ShowEmpty);
        }
        Ok(beads.remove(0))
    }

    /// `bd create --silent` → newly created bead id.
    ///
    /// Uses `--silent` (which prints only the id) to dodge the JSON
    /// deserializer for a single field. The id is the only output the
    /// caller needs at this point.
    pub async fn create(&self, opts: CreateOpts) -> Result<BeadId, BdError> {
        let mut args: Vec<OsString> = vec![
            "create".into(),
            "--silent".into(),
            "--title".into(),
            opts.title.into(),
            "--description".into(),
            opts.description.into(),
        ];
        if let Some(t) = opts.issue_type {
            args.push("--type".into());
            args.push(t.into());
        }
        if let Some(p) = opts.priority {
            args.push("--priority".into());
            args.push(p.to_string().into());
        }
        if let Some(parent) = opts.parent {
            args.push("--parent".into());
            args.push(parent.as_str().to_owned().into());
        }
        if !opts.labels.is_empty() {
            args.push("--labels".into());
            args.push(opts.labels.join(",").into());
        }
        let out = self.invoke(args).await?;
        let stdout = String::from_utf8(out.stdout)?;
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            return Err(BdError::CreateMissingId);
        }
        Ok(BeadId::new(trimmed)?)
    }

    /// `bd close <id>` (optionally with `--reason`).
    pub async fn close(&self, id: &BeadId, reason: Option<&str>) -> Result<(), BdError> {
        let mut args: Vec<OsString> = vec!["close".into(), id.as_str().to_owned().into()];
        if let Some(r) = reason {
            args.push("--reason".into());
            args.push(r.to_owned().into());
        }
        self.invoke(args).await?;
        Ok(())
    }

    /// `bd update <id> [flags]`. Flags map onto the corresponding `bd
    /// update` switches; unset fields are not forwarded.
    pub async fn update(&self, id: &BeadId, opts: UpdateOpts) -> Result<(), BdError> {
        let mut args: Vec<OsString> = vec!["update".into(), id.as_str().to_owned().into()];
        if opts.claim {
            args.push("--claim".into());
        }
        if let Some(s) = opts.status {
            args.push("--status".into());
            args.push(s.into());
        }
        if let Some(p) = opts.priority {
            args.push("--priority".into());
            args.push(p.to_string().into());
        }
        for label in opts.add_labels {
            args.push("--add-label".into());
            args.push(label.into());
        }
        for label in opts.remove_labels {
            args.push("--remove-label".into());
            args.push(label.into());
        }
        self.invoke(args).await?;
        Ok(())
    }

    /// `bd list --json` filtered by status and/or label.
    pub async fn list(&self, opts: ListOpts) -> Result<Vec<Bead>, BdError> {
        let mut args: Vec<OsString> = vec!["list".into(), "--json".into()];
        if let Some(status) = opts.status {
            args.push(format!("--status={status}").into());
        }
        if let Some(label) = opts.label {
            args.push(format!("--label={label}").into());
        }
        let out = self.invoke(args).await?;
        // `bd list --json` returns `null` when the result set is empty.
        if out.stdout.iter().all(u8::is_ascii_whitespace) {
            return Ok(Vec::new());
        }
        let trimmed = std::str::from_utf8(&out.stdout)
            .map(str::trim)
            .unwrap_or_default();
        if trimmed == "null" {
            return Ok(Vec::new());
        }
        decode(&out.stdout, &out.args)
    }

    /// `bd dep add <issue> <depends-on>`.
    pub async fn dep_add(&self, issue: &BeadId, depends_on: &BeadId) -> Result<(), BdError> {
        let args = args(["dep", "add", issue.as_str(), depends_on.as_str()]);
        self.invoke(args).await?;
        Ok(())
    }

    /// `bd ready --json [--limit=N] [--label=<label>]` — beads ready to work
    /// (open, no active blockers). Step (1) of the parallel batch driver:
    /// pulls up to `limit` candidates per batch.
    pub async fn ready(&self, opts: ReadyOpts) -> Result<Vec<Bead>, BdError> {
        let mut args: Vec<OsString> = vec!["ready".into(), "--json".into()];
        if let Some(n) = opts.limit {
            args.push(format!("--limit={n}").into());
        }
        if let Some(label) = opts.label {
            args.push(format!("--label={label}").into());
        }
        let out = self.invoke(args).await?;
        if out.stdout.iter().all(u8::is_ascii_whitespace) {
            return Ok(Vec::new());
        }
        let trimmed = std::str::from_utf8(&out.stdout)
            .map(str::trim)
            .unwrap_or_default();
        if trimmed == "null" {
            return Ok(Vec::new());
        }
        decode(&out.stdout, &out.args)
    }

    /// `bd mol bond <left> <right>`. The polymorphic semantics of
    /// `bd mol bond` (formula+formula, formula+mol, etc.) are the
    /// caller's concern; this wrapper just forwards two operands.
    pub async fn mol_bond(&self, left: &str, right: &str) -> Result<(), BdError> {
        let args = args(["mol", "bond", left, right]);
        self.invoke(args).await?;
        Ok(())
    }

    /// `bd mol progress <id> --json`.
    pub async fn mol_progress(&self, id: &MoleculeId) -> Result<MolProgress, BdError> {
        let args = args(["mol", "progress", id.as_str(), "--json"]);
        let out = self.invoke(args).await?;
        decode(&out.stdout, &out.args)
    }

    async fn invoke(&self, args: Vec<OsString>) -> Result<Invocation, BdError> {
        let rendered = render_args(&args);
        let output: RunOutput = self.runner.run(args, self.timeout).await?;
        if !output.success() {
            return Err(BdError::Cli {
                status: output.status,
                args: rendered.clone(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(Invocation {
            stdout: output.stdout,
            args: rendered,
        })
    }
}

struct Invocation {
    stdout: Vec<u8>,
    args: String,
}

/// Borrow-friendly converter from `&[&str]` to `Vec<OsString>`. Used at
/// every call site that doesn't need to splice agent-supplied strings —
/// agent input still arrives via owned `OsString` to make the
/// no-shell-interpolation guarantee explicit.
fn args<const N: usize>(strs: [&str; N]) -> Vec<OsString> {
    strs.iter().map(|s| OsString::from(*s)).collect()
}

fn decode<T: for<'de> Deserialize<'de>>(stdout: &[u8], args: &str) -> Result<T, BdError> {
    serde_json::from_slice(stdout).map_err(|source| BdError::Decode {
        args: args.to_owned(),
        source,
    })
}

/// Fields accepted by `bd create`. Only fields the workflow actually sets
/// today are modelled; extend as new call sites need them.
#[derive(Debug, Clone, Default)]
pub struct CreateOpts {
    pub title: String,
    pub description: String,
    pub issue_type: Option<String>,
    pub priority: Option<u8>,
    pub labels: Vec<String>,
    pub parent: Option<BeadId>,
}

/// Fields accepted by `bd update`. Empty defaults forward nothing.
#[derive(Debug, Clone, Default)]
pub struct UpdateOpts {
    pub claim: bool,
    pub status: Option<String>,
    pub priority: Option<u8>,
    pub add_labels: Vec<String>,
    pub remove_labels: Vec<String>,
}

/// Filters accepted by `bd list`. Both fields are optional; passing
/// neither lists every open bead (matching the CLI default).
#[derive(Debug, Clone, Default)]
pub struct ListOpts {
    pub status: Option<String>,
    pub label: Option<String>,
}

/// Filters accepted by `bd ready`. `limit` caps the result count
/// (`--limit=N`); the parallel batch driver uses it to pull at most N ready
/// beads per batch.
#[derive(Debug, Clone, Default)]
pub struct ReadyOpts {
    pub limit: Option<u32>,
    pub label: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::bd::Label;
    use anyhow::{Result, anyhow};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Default)]
    struct CapturingRunner {
        responses: Mutex<VecDeque<RunOutput>>,
        calls: Mutex<Vec<Vec<OsString>>>,
    }

    impl CapturingRunner {
        fn new(responses: impl IntoIterator<Item = RunOutput>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl CommandRunner for CapturingRunner {
        async fn run(
            &self,
            args: Vec<OsString>,
            _t: Duration,
        ) -> std::result::Result<RunOutput, BdError> {
            let mut calls = self.calls.lock().unwrap_or_else(|p| p.into_inner());
            calls.push(args);
            let mut responses = self.responses.lock().unwrap_or_else(|p| p.into_inner());
            Ok(responses.pop_front().unwrap_or(RunOutput {
                status: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
            }))
        }
    }

    fn argv_of(runner: &CapturingRunner, idx: usize) -> Vec<String> {
        let calls = runner.calls.lock().unwrap_or_else(|p| p.into_inner());
        calls[idx]
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    fn ok(stdout: &[u8]) -> RunOutput {
        RunOutput {
            status: 0,
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        }
    }

    fn fail(status: i32, stderr: &str) -> RunOutput {
        RunOutput {
            status,
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    const SHOW_FIXTURE: &str = r#"[
      {
        "id": "wx-3hhwq.5",
        "title": "BdClient",
        "description": "Implement BdClient",
        "status": "in_progress",
        "priority": 2,
        "issue_type": "task",
        "labels": ["profile:rust", "spec:loom-harness"]
      }
    ]"#;

    #[tokio::test]
    async fn show_parses_first_row_into_bead() -> Result<()> {
        let runner = CapturingRunner::new([ok(SHOW_FIXTURE.as_bytes())]);
        let client = BdClient::with_runner(runner);
        let bead = client.show(&BeadId::new("wx-3hhwq.5")?).await?;
        assert_eq!(bead.id, BeadId::new("wx-3hhwq.5")?);
        assert_eq!(bead.title, "BdClient");
        assert_eq!(bead.status, "in_progress");
        assert_eq!(bead.priority, 2);
        assert_eq!(bead.issue_type, "task");
        assert_eq!(
            bead.labels,
            vec![Label::new("profile:rust"), Label::new("spec:loom-harness")]
        );
        let argv = argv_of(&client.runner, 0);
        assert_eq!(argv, vec!["show", "wx-3hhwq.5", "--json"]);
        Ok(())
    }

    #[tokio::test]
    async fn show_returns_show_empty_for_zero_rows() -> Result<()> {
        let runner = CapturingRunner::new([ok(b"[]")]);
        let client = BdClient::with_runner(runner);
        let err = client
            .show(&BeadId::new("wx-missing")?)
            .await
            .err()
            .ok_or_else(|| anyhow!("empty array must error"))?;
        assert!(matches!(err, BdError::ShowEmpty), "got {err:?}");
        Ok(())
    }

    #[tokio::test]
    async fn list_filters_status_and_label() -> Result<()> {
        let runner = CapturingRunner::new([ok(b"[]")]);
        let client = BdClient::with_runner(runner);
        client
            .list(ListOpts {
                status: Some("open".into()),
                label: Some("spec:loom-harness".into()),
            })
            .await?;
        let argv = argv_of(&client.runner, 0);
        assert_eq!(
            argv,
            vec![
                "list".to_string(),
                "--json".into(),
                "--status=open".into(),
                "--label=spec:loom-harness".into(),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn list_handles_null_response_as_empty_vec() -> Result<()> {
        let runner = CapturingRunner::new([ok(b"null\n")]);
        let client = BdClient::with_runner(runner);
        let beads = client.list(ListOpts::default()).await?;
        assert!(beads.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn list_parses_array_of_beads() -> Result<()> {
        let json = br#"[
            {"id":"wx-a","title":"A","status":"open"},
            {"id":"wx-b","title":"B","status":"closed"}
        ]"#;
        let runner = CapturingRunner::new([ok(json)]);
        let client = BdClient::with_runner(runner);
        let beads = client.list(ListOpts::default()).await?;
        assert_eq!(beads.len(), 2);
        assert_eq!(beads[0].id, BeadId::new("wx-a")?);
        assert_eq!(beads[1].id, BeadId::new("wx-b")?);
        Ok(())
    }

    #[tokio::test]
    async fn create_returns_id_from_silent_output() -> Result<()> {
        let runner = CapturingRunner::new([ok(b"wx-new.7\n")]);
        let client = BdClient::with_runner(runner);
        let id = client
            .create(CreateOpts {
                title: "do thing".into(),
                description: "why".into(),
                issue_type: Some("task".into()),
                priority: Some(2),
                labels: vec!["profile:rust".into()],
                parent: Some(BeadId::new("wx-3hhwq")?),
            })
            .await?;
        assert_eq!(id, BeadId::new("wx-new.7")?);
        let argv = argv_of(&client.runner, 0);
        assert!(argv.starts_with(&["create".to_string(), "--silent".into()]));
        assert!(argv.contains(&"--title".to_string()));
        assert!(argv.contains(&"do thing".to_string()));
        assert!(argv.contains(&"--type".to_string()));
        assert!(argv.contains(&"task".to_string()));
        assert!(argv.contains(&"--parent".to_string()));
        assert!(argv.contains(&"wx-3hhwq".to_string()));
        assert!(argv.contains(&"--labels".to_string()));
        assert!(argv.contains(&"profile:rust".to_string()));
        Ok(())
    }

    #[tokio::test]
    async fn create_rejects_malformed_silent_output() -> Result<()> {
        let runner = CapturingRunner::new([ok(b"warning: foo\nwx-abc123\n")]);
        let client = BdClient::with_runner(runner);
        let err = client
            .create(CreateOpts {
                title: "x".into(),
                description: "y".into(),
                ..CreateOpts::default()
            })
            .await
            .err()
            .ok_or_else(|| anyhow!("banner-prefixed stdout must error"))?;
        assert!(matches!(err, BdError::CreateInvalidId(_)), "got {err:?}");
        Ok(())
    }

    #[tokio::test]
    async fn create_errors_on_blank_silent_output() -> Result<()> {
        let runner = CapturingRunner::new([ok(b"\n")]);
        let client = BdClient::with_runner(runner);
        let err = client
            .create(CreateOpts {
                title: "x".into(),
                description: "y".into(),
                ..CreateOpts::default()
            })
            .await
            .err()
            .ok_or_else(|| anyhow!("blank stdout must error"))?;
        assert!(matches!(err, BdError::CreateMissingId));
        Ok(())
    }

    #[tokio::test]
    async fn cli_failure_maps_to_typed_error() -> Result<()> {
        let runner = CapturingRunner::new([fail(1, "no issue found matching \"wx-miss\"")]);
        let client = BdClient::with_runner(runner);
        let err = client
            .show(&BeadId::new("wx-miss")?)
            .await
            .err()
            .ok_or_else(|| anyhow!("non-zero exit must surface"))?;
        let BdError::Cli {
            status,
            args,
            stderr,
        } = err
        else {
            return Err(anyhow!("expected BdError::Cli"));
        };
        assert_eq!(status, 1);
        assert!(args.contains("show"));
        assert!(args.contains("wx-miss"));
        assert!(stderr.contains("no issue found"));
        Ok(())
    }

    #[tokio::test]
    async fn decode_failure_carries_args_context() -> Result<()> {
        let runner = CapturingRunner::new([ok(b"not json")]);
        let client = BdClient::with_runner(runner);
        let err = client
            .show(&BeadId::new("wx-x")?)
            .await
            .err()
            .ok_or_else(|| anyhow!("garbage stdout must fail"))?;
        let BdError::Decode { args, .. } = err else {
            return Err(anyhow!("expected BdError::Decode"));
        };
        assert!(args.contains("show"));
        assert!(args.contains("wx-x"));
        Ok(())
    }

    #[tokio::test]
    async fn update_emits_only_set_fields() -> Result<()> {
        let runner = CapturingRunner::new([ok(b"")]);
        let client = BdClient::with_runner(runner);
        client
            .update(
                &BeadId::new("wx-3hhwq.5")?,
                UpdateOpts {
                    claim: true,
                    status: Some("in_progress".into()),
                    add_labels: vec!["urgent".into()],
                    ..UpdateOpts::default()
                },
            )
            .await?;
        let argv = argv_of(&client.runner, 0);
        assert_eq!(argv[0], "update");
        assert_eq!(argv[1], "wx-3hhwq.5");
        assert!(argv.contains(&"--claim".to_string()));
        assert!(argv.contains(&"--status".to_string()));
        assert!(argv.contains(&"in_progress".to_string()));
        assert!(argv.contains(&"--add-label".to_string()));
        assert!(argv.contains(&"urgent".to_string()));
        assert!(!argv.contains(&"--priority".to_string()));
        assert!(!argv.contains(&"--remove-label".to_string()));
        Ok(())
    }

    #[tokio::test]
    async fn close_with_reason_passes_flag() -> Result<()> {
        let runner = CapturingRunner::new([ok(b"")]);
        let client = BdClient::with_runner(runner);
        client.close(&BeadId::new("wx-x")?, Some("dup")).await?;
        let argv = argv_of(&client.runner, 0);
        assert_eq!(argv, vec!["close", "wx-x", "--reason", "dup"]);
        Ok(())
    }

    #[tokio::test]
    async fn ready_forwards_limit_and_label_filters() -> Result<()> {
        let runner = CapturingRunner::new([ok(b"[]")]);
        let client = BdClient::with_runner(runner);
        client
            .ready(ReadyOpts {
                limit: Some(4),
                label: Some("spec:loom-harness".into()),
            })
            .await?;
        let argv = argv_of(&client.runner, 0);
        assert_eq!(
            argv,
            vec![
                "ready".to_string(),
                "--json".into(),
                "--limit=4".into(),
                "--label=spec:loom-harness".into(),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn ready_handles_null_response_as_empty_vec() -> Result<()> {
        let runner = CapturingRunner::new([ok(b"null\n")]);
        let client = BdClient::with_runner(runner);
        let beads = client.ready(ReadyOpts::default()).await?;
        assert!(beads.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn ready_parses_array_of_beads() -> Result<()> {
        let json = br#"[
            {"id":"wx-a","title":"A","status":"open"},
            {"id":"wx-b","title":"B","status":"open"}
        ]"#;
        let runner = CapturingRunner::new([ok(json)]);
        let client = BdClient::with_runner(runner);
        let beads = client
            .ready(ReadyOpts {
                limit: Some(2),
                label: None,
            })
            .await?;
        assert_eq!(beads.len(), 2);
        assert_eq!(beads[0].id, BeadId::new("wx-a")?);
        assert_eq!(beads[1].id, BeadId::new("wx-b")?);
        Ok(())
    }

    #[tokio::test]
    async fn dep_add_argv_is_positional() -> Result<()> {
        let runner = CapturingRunner::new([ok(b"")]);
        let client = BdClient::with_runner(runner);
        client
            .dep_add(&BeadId::new("wx-a")?, &BeadId::new("wx-b")?)
            .await?;
        let argv = argv_of(&client.runner, 0);
        assert_eq!(argv, vec!["dep", "add", "wx-a", "wx-b"]);
        Ok(())
    }

    #[tokio::test]
    async fn mol_progress_parses_progress_object() -> Result<()> {
        let json = br#"{
          "molecule_id": "wx-3hhwq",
          "molecule_title": "Loom harness",
          "completed": 8,
          "in_progress": 1,
          "total": 19,
          "percent": 42.1,
          "current_step_id": "wx-3hhwq.5"
        }"#;
        let runner = CapturingRunner::new([ok(json)]);
        let client = BdClient::with_runner(runner);
        let progress = client.mol_progress(&MoleculeId::new("wx-3hhwq")).await?;
        assert_eq!(progress.molecule_id, MoleculeId::new("wx-3hhwq"));
        assert_eq!(progress.completed, 8);
        assert_eq!(progress.total, 19);
        assert_eq!(progress.current_step_id.as_deref(), Some("wx-3hhwq.5"));
        Ok(())
    }
}
