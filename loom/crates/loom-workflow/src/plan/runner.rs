use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tracing::info;

use loom_core::config::{ExitSignalsConfig, LoomConfig};
use loom_core::identifier::SpecLabel;
use loom_core::lock::LockManager;
use loom_core::state::StateDb;

use super::args::PlanMode;
use super::command::{WRAPIX_BIN, build_wrapix_argv};
use super::companions::reconcile_companions;
use super::error::PlanError;
use super::prompt::{PlanPromptInputs, render_prompt};

/// Default timeout used by [`run`] — mirrors the rest of the spec-scoped
/// command surface (see `LockManager::acquire_spec`).
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Options accepted by [`run`].
pub struct PlanOpts {
    pub mode: PlanMode,
    /// Explicit path to the `wrapix` launcher. `None` falls back to
    /// [`WRAPIX_BIN`] on `PATH`. Tests pass a stub here.
    pub wrapix_bin: Option<PathBuf>,
}

/// Files touched by [`run`]. Surfaces the resolved spec path and the
/// reconciled companion paths so the binary can print a useful summary.
#[derive(Debug, Clone)]
pub struct PlanReport {
    pub label: SpecLabel,
    pub spec_path: PathBuf,
    pub companion_paths: Vec<String>,
    /// `true` when the spec markdown contained a `## Companions` heading.
    /// `false` lets the CLI distinguish "intentionally empty" from "interview
    /// did not declare any" in the human-readable summary.
    pub companions_section_present: bool,
}

/// Run `loom plan` against `workspace`.
///
/// 1. Acquire `<label>.lock` for the duration of the call.
/// 2. Render the appropriate Askama template into a prompt body.
/// 3. Spawn `wrapix run <workspace> claude --dangerously-skip-permissions
///    <prompt>` with stdio inherited and wait for it to exit.
/// 4. After the interactive session exits, replace the companion rows for
///    `label` in the state DB by re-parsing the spec file.
pub fn run(workspace: &Path, opts: PlanOpts) -> Result<PlanReport, PlanError> {
    run_with_timeout(workspace, opts, DEFAULT_LOCK_TIMEOUT)
}

/// Same as [`run`] with an explicit lock-wait timeout. Tests use this to
/// keep the contention path fast.
pub fn run_with_timeout(
    workspace: &Path,
    opts: PlanOpts,
    timeout: Duration,
) -> Result<PlanReport, PlanError> {
    let label = opts.mode.label().clone();
    let is_new = matches!(opts.mode, PlanMode::New(_));

    let lock_mgr = LockManager::new(workspace)?;
    let _guard = lock_mgr.acquire_spec_with_timeout(&label, timeout)?;

    let cfg = LoomConfig::load(workspace.join(".wrapix/loom/config.toml"))
        .unwrap_or_else(|_| LoomConfig::default());

    let spec_rel = format!("specs/{}.md", label.as_str());
    let spec_path = workspace.join(&spec_rel);

    if !is_new && !spec_path.exists() {
        return Err(PlanError::SpecMissing {
            path: spec_path.clone(),
        });
    }

    let pinned_context = read_pinned_context(workspace, &cfg.pinned_context)?;
    let exit_signals = render_exit_signals(&cfg.exit_signals);

    let db = StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let companion_paths = if is_new {
        Vec::new()
    } else {
        db.companions(&label)?
    };

    let prompt_body = render_prompt(PlanPromptInputs {
        mode: opts.mode,
        spec_path: spec_rel.clone(),
        pinned_context,
        companion_paths,
        exit_signals,
    })?;

    let argv = build_wrapix_argv(workspace, &prompt_body);
    let bin: PathBuf = opts.wrapix_bin.unwrap_or_else(|| PathBuf::from(WRAPIX_BIN));
    info!(label = %label, wrapix_bin = %bin.display(), "loom plan: shelling out to interactive wrapix run");
    let status = Command::new(&bin)
        .args(&argv)
        .status()
        .map_err(|source| PlanError::Spawn { source })?;
    if !status.success() {
        return Err(PlanError::WrapixExit {
            status: status.to_string(),
        });
    }

    if is_new && !spec_path.exists() {
        return Err(PlanError::InterviewProducedNoSpec {
            path: spec_path.clone(),
        });
    }

    let outcome = reconcile_companions(&db, &label, &spec_path)?;

    Ok(PlanReport {
        label,
        spec_path,
        companion_paths: outcome.paths,
        companions_section_present: outcome.section_present,
    })
}

fn read_pinned_context(workspace: &Path, rel: &str) -> Result<String, PlanError> {
    let path = workspace.join(rel);
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(source) => Err(PlanError::ReadPinnedContext { path, source }),
    }
}

fn render_exit_signals(cfg: &ExitSignalsConfig) -> String {
    format!(
        "- `{}`\n- `{}`\n- `{}`",
        cfg.complete, cfg.blocked, cfg.clarify
    )
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests use panicking helpers")]
mod tests {
    use super::*;
    use anyhow::Result;
    use loom_core::lock::LockError;
    use std::os::unix::fs::PermissionsExt;

    /// Write a stub `wrapix` shell launcher under `dir/bin/`, recording argv
    /// to `dir/argv.log`, and return the absolute binary path. The script
    /// touches `<workspace>/specs/<label>.md` so post-session companion
    /// reconciliation finds a file to read — mirroring what claude would
    /// have written during the interview.
    fn install_wrapix_stub(
        dir: &Path,
        post_session_spec: Option<(&Path, &str)>,
    ) -> Result<PathBuf> {
        let bin_dir = dir.join("bin");
        std::fs::create_dir_all(&bin_dir)?;
        let bin = bin_dir.join("wrapix-stub");
        let log = dir.join("argv.log");
        let mut script = format!(
            "#!/bin/sh\nset -e\n\
             # log argv one-per-line so tests can grep for individual flags\n\
             for a in \"$@\"; do printf '%s\\n' \"$a\" >> {log:?}; done\n\
             printf -- '---\\n' >> {log:?}\n",
            log = log
        );
        if let Some((spec, body)) = post_session_spec {
            let parent = spec.parent().unwrap().to_path_buf();
            script.push_str(&format!(
                "mkdir -p {parent:?}\ncat > {spec:?} <<'WRAPIX_EOF'\n{body}\nWRAPIX_EOF\n",
            ));
        }
        std::fs::write(&bin, script)?;
        let mut perm = std::fs::metadata(&bin)?.permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&bin, perm)?;
        Ok(bin)
    }

    fn workspace_with_specs() -> Result<tempfile::TempDir> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("specs"))?;
        std::fs::create_dir_all(dir.path().join(".wrapix/loom"))?;
        // Seed the state DB so the runner can replace_companions afterwards.
        let _seed = StateDb::open(dir.path().join(".wrapix/loom/state.db"))?;
        Ok(dir)
    }

    #[test]
    fn plan_new_invokes_wrapix_run_and_records_companions() -> Result<()> {
        let dir = workspace_with_specs()?;
        let spec_path = dir.path().join("specs/loom-harness.md");
        let bin = install_wrapix_stub(
            dir.path(),
            Some((
                &spec_path,
                "# loom-harness\n\n## Companions\n\n- `lib/sandbox/`\n",
            )),
        )?;

        let report = run_with_timeout(
            dir.path(),
            PlanOpts {
                mode: PlanMode::New(SpecLabel::new("loom-harness")),
                wrapix_bin: Some(bin),
            },
            Duration::from_millis(100),
        )?;

        assert_eq!(report.label.as_str(), "loom-harness");
        assert_eq!(report.companion_paths, vec!["lib/sandbox/"]);
        assert!(report.companions_section_present);

        let argv_log = std::fs::read_to_string(dir.path().join("argv.log"))?;
        let lines: Vec<&str> = argv_log.lines().collect();
        assert_eq!(lines[0], "run", "first argv must be `run`");
        assert!(!lines.contains(&"run-bead"));
        assert!(!lines.contains(&"--stdio"));
        assert!(!lines.contains(&"--spawn-config"));
        assert!(lines.contains(&"claude"));
        assert!(lines.contains(&"--dangerously-skip-permissions"));
        Ok(())
    }

    #[test]
    fn plan_update_threads_existing_companions_into_prompt() -> Result<()> {
        let dir = workspace_with_specs()?;
        let spec_path = dir.path().join("specs/loom-harness.md");
        std::fs::write(
            &spec_path,
            "# loom-harness\n\n## Companions\n\n- `lib/sandbox/`\n",
        )?;
        let db = StateDb::open(dir.path().join(".wrapix/loom/state.db"))?;
        db.replace_companions(
            &SpecLabel::new("loom-harness"),
            &spec_path,
            &["lib/sandbox/".to_string()],
        )?;
        drop(db);

        let bin = install_wrapix_stub(
            dir.path(),
            Some((
                &spec_path,
                "# loom-harness\n\n## Companions\n\n- `lib/sandbox/`\n- `lib/ralph/template/`\n",
            )),
        )?;

        let report = run_with_timeout(
            dir.path(),
            PlanOpts {
                mode: PlanMode::Update(SpecLabel::new("loom-harness")),
                wrapix_bin: Some(bin),
            },
            Duration::from_millis(100),
        )?;

        assert_eq!(
            report.companion_paths,
            vec!["lib/sandbox/", "lib/ralph/template/"]
        );

        let argv_log = std::fs::read_to_string(dir.path().join("argv.log"))?;
        assert!(argv_log.contains("# Specification Update Interview"));
        assert!(argv_log.contains("- lib/sandbox/"));
        Ok(())
    }

    #[test]
    fn plan_update_errors_when_spec_missing() -> Result<()> {
        let dir = workspace_with_specs()?;
        let result = run_with_timeout(
            dir.path(),
            PlanOpts {
                mode: PlanMode::Update(SpecLabel::new("loom-harness")),
                wrapix_bin: Some(PathBuf::from("/nonexistent/wrapix")),
            },
            Duration::from_millis(100),
        );
        match result {
            Err(PlanError::SpecMissing { path }) => {
                assert!(path.ends_with("specs/loom-harness.md"));
                Ok(())
            }
            other => Err(anyhow::anyhow!("expected SpecMissing, got {other:?}")),
        }
    }

    #[test]
    fn plan_new_errors_when_interview_writes_no_spec() -> Result<()> {
        let dir = workspace_with_specs()?;
        // Stub exits 0 without writing the spec — mimics the agent quitting
        // the interview without saving the file.
        let bin = install_wrapix_stub(dir.path(), None)?;

        let result = run_with_timeout(
            dir.path(),
            PlanOpts {
                mode: PlanMode::New(SpecLabel::new("loom-harness")),
                wrapix_bin: Some(bin),
            },
            Duration::from_millis(100),
        );
        match result {
            Err(PlanError::InterviewProducedNoSpec { path }) => {
                assert!(path.ends_with("specs/loom-harness.md"));
                Ok(())
            }
            other => Err(anyhow::anyhow!(
                "expected InterviewProducedNoSpec, got {other:?}"
            )),
        }
    }

    #[test]
    fn plan_new_flags_missing_companions_section() -> Result<()> {
        let dir = workspace_with_specs()?;
        let spec_path = dir.path().join("specs/loom-harness.md");
        // Spec written, but the agent did not include `## Companions`.
        let bin = install_wrapix_stub(
            dir.path(),
            Some((&spec_path, "# loom-harness\n\nNo companions yet.\n")),
        )?;

        let report = run_with_timeout(
            dir.path(),
            PlanOpts {
                mode: PlanMode::New(SpecLabel::new("loom-harness")),
                wrapix_bin: Some(bin),
            },
            Duration::from_millis(100),
        )?;

        assert!(report.companion_paths.is_empty());
        assert!(!report.companions_section_present);
        Ok(())
    }

    #[test]
    fn plan_acquires_per_spec_lock() -> Result<()> {
        let dir = workspace_with_specs()?;
        let mgr = LockManager::new(dir.path())?;
        let _hold = mgr.acquire_spec(&SpecLabel::new("alpha"))?;

        match run_with_timeout(
            dir.path(),
            PlanOpts {
                mode: PlanMode::New(SpecLabel::new("alpha")),
                wrapix_bin: Some(PathBuf::from("/nonexistent/wrapix")),
            },
            Duration::from_millis(100),
        ) {
            Err(PlanError::Lock(LockError::SpecBusy { label })) => {
                assert_eq!(label, "alpha");
                Ok(())
            }
            other => Err(anyhow::anyhow!("expected SpecBusy, got {other:?}")),
        }
    }
}
