//! `loom` CLI binary entry point.
//!
//! Parses command-line arguments and dispatches to the workflow modules in
//! `loom-workflow`. The set of subcommands matches the harness specification:
//! `init`, `status`, `use`, `logs`, `spec`, plus the previously-implemented
//! `run`, `check`, `msg`. There is no `sync` or `tune` — Askama compiled
//! templates make per-project sync unnecessary (see `specs/loom-harness.md`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};

use loom_agent::{ClaudeBackend, PiBackend};
use loom_driver::agent::{AgentKind, LOOM_INSIDE_ENV, ProtocolError, SessionOutcome, SpawnConfig};
use loom_driver::bd::{BdClient, ListOpts, UpdateOpts};
use loom_driver::clock::{Clock, SystemClock};
use loom_driver::config::{LoomConfig, Phase};
use loom_driver::git::GitClient;
use loom_driver::identifier::{BeadId, ProfileName, SpecLabel};
use loom_driver::lock::LockManager;
use loom_driver::logging::{LogSink, sweep_retention_at};
use loom_driver::profile_manifest::ProfileImageManifest;
use loom_driver::scratch::resolve_scratch_key;
use loom_driver::state::StateDb;
use loom_workflow::check::{IterationCap, ProductionCheckController, check_loop as run_check_loop};
use loom_workflow::msg::{
    DISMISS_NOTE, build_rows, compose_option_note, filter_msg_beads, kind_of, resolve_target,
    spec_label_of,
};
use loom_workflow::run::{
    Parallelism, ProductionAgentLoopController, RetryPolicy, RunMode, SessionResult, run_loop,
};
use loom_workflow::todo::{
    ExitSignal, ProductionTodoController, parse_exit_signal, run as run_todo_workflow,
};
use loom_workflow::{doctor, init, logs_cmd, plan, spec, status, use_spec};
use loom_workflow::{run_agent, run_agent_classified};

/// Top-level CLI surface.
#[derive(Debug, Parser)]
#[command(name = "loom", version, about = "Loom harness CLI")]
struct Cli {
    /// Workspace root. Defaults to the current working directory.
    #[arg(long, short = 'w', global = true, value_name = "PATH")]
    workspace: Option<PathBuf>,

    /// Override the agent backend for this invocation.
    #[arg(long, short = 'A', global = true, value_enum, value_name = "BACKEND")]
    agent: Option<AgentBackendArg>,

    #[command(subcommand)]
    command: Command,
}

/// CLI surface for `--agent`. Maps one-to-one with [`AgentKind`] so the
/// dispatcher does not need to re-parse strings — clap's value-enum
/// validation owns the rejection of unknown names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
enum AgentBackendArg {
    Claude,
    Pi,
}

impl From<AgentBackendArg> for AgentKind {
    fn from(arg: AgentBackendArg) -> Self {
        match arg {
            AgentBackendArg::Claude => AgentKind::Claude,
            AgentBackendArg::Pi => AgentKind::Pi,
        }
    }
}

/// Subcommands of `loom note`.
#[derive(Debug, Subcommand)]
enum NoteAction {
    /// Atomically replace every note for `<label>` under `--kind`.
    Set {
        label: String,
        /// JSON array of note strings: `'["note 1", "note 2"]'`.
        #[arg(long)]
        json: String,
        /// Note kind (default `implementation`).
        #[arg(long, default_value = "implementation")]
        kind: String,
    },
    /// Append a single note.
    Add {
        label: String,
        #[arg(long)]
        text: String,
        #[arg(long, default_value = "implementation")]
        kind: String,
    },
    /// Delete notes for `<label>`. By default just the
    /// `--kind implementation` rows; `--all-kinds` widens to every kind.
    Clear {
        label: String,
        #[arg(long, default_value = "implementation", conflicts_with = "all_kinds")]
        kind: String,
        #[arg(long)]
        all_kinds: bool,
    },
    /// List notes by `(label, kind)`. Omitting `<label>` widens to
    /// every spec; `--all-kinds` widens beyond the default kind.
    List {
        label: Option<String>,
        #[arg(long, default_value = "implementation", conflicts_with = "all_kinds")]
        kind: String,
        #[arg(long)]
        all_kinds: bool,
    },
    /// Remove a single note by its row id.
    Rm { id: i64 },
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize the workspace (create `.wrapix/loom/` config + state DB).
    #[command(next_help_heading = "Workspace")]
    Init {
        /// Drop and repopulate the state DB from `specs/*.md` and active beads.
        #[arg(long)]
        rebuild: bool,
    },
    /// Print the active spec, current molecule, and iteration counter.
    #[command(next_help_heading = "Inspect")]
    Status,
    /// Set the active spec.
    #[command(name = "use", next_help_heading = "Workspace")]
    UseSpec {
        /// Spec label (matches `<workspace>/specs/<label>.md`).
        label: String,
    },
    /// Tail the most recent per-bead JSONL log.
    #[command(next_help_heading = "Inspect")]
    Logs {
        /// Restrict the search to a specific bead id.
        #[arg(long)]
        bead: Option<String>,
    },
    /// Inspect spec annotations and tooling dependencies.
    #[command(next_help_heading = "Inspect")]
    Spec {
        /// Print the unique nixpkgs names referenced by verify/judge tests.
        #[arg(long)]
        deps: bool,
    },
    /// Interactive spec interview (`-n <label>` new, `-u <label>` update).
    #[command(next_help_heading = "Workflow")]
    Plan {
        /// New-spec interview for `<label>`.
        #[arg(short = 'n', value_name = "LABEL")]
        new: Option<String>,
        /// Update-spec interview for `<label>`.
        #[arg(short = 'u', value_name = "LABEL")]
        update: Option<String>,
        /// Override the profile resolution chain. Wins over
        /// `[phase.plan].profile` and `[phase.default].profile` in
        /// `.wrapix/loom/config.toml` (default `base`).
        #[arg(long, value_name = "PROFILE")]
        profile: Option<String>,
    },
    /// Per-bead execution loop. Continuous by default; `--once` exits after one bead.
    #[command(next_help_heading = "Workflow")]
    Run {
        /// Process a single bead then exit (no auto-handoff to `loom check`).
        #[arg(long)]
        once: bool,
        /// Concurrent dispatch slots (`-p N` / `--parallel N`). Default 1.
        #[arg(long, short = 'p', default_value = "1")]
        parallel: Parallelism,
        /// Override the per-bead `profile:X` label resolution.
        #[arg(long, value_name = "PROFILE")]
        profile: Option<String>,
        /// Spec label override (defaults to `current_spec`).
        #[arg(long, short = 's', value_name = "LABEL")]
        spec: Option<String>,
    },
    /// Post-loop reviewer + push gate.
    #[command(next_help_heading = "Workflow")]
    Check {
        /// Spec label override (defaults to `current_spec`).
        #[arg(long, short = 's', value_name = "LABEL")]
        spec: Option<String>,
    },
    /// Resolve outstanding `loom:clarify` and `loom:blocked` beads.
    #[command(next_help_heading = "Workflow")]
    Msg {
        /// Filter to a specific spec label.
        #[arg(long, short = 's', value_name = "LABEL")]
        spec: Option<String>,
        /// Select bead by 1-based index in the printed list. Mutually
        /// exclusive with `-b`.
        #[arg(long, short = 'n', value_name = "N", conflicts_with = "bead")]
        number: Option<u32>,
        /// Select bead by id. Mutually exclusive with `-n`.
        #[arg(long, short = 'b', value_name = "ID")]
        bead: Option<String>,
        /// Fast-reply with the body of `### Option <int>` for a clarify
        /// bead. Validated — missing subsection exits non-zero before any
        /// bd state is mutated. Mutually exclusive with `-r` and `-d`.
        #[arg(
            long,
            short = 'o',
            value_name = "INT",
            conflicts_with_all = ["reply", "dismiss"]
        )]
        option: Option<u32>,
        /// Fast-reply with verbatim text. Works on any bead regardless of
        /// whether it has an `## Options` section. Mutually exclusive with
        /// `-o` and `-d`.
        #[arg(
            long,
            short = 'r',
            value_name = "TEXT",
            conflicts_with_all = ["option", "dismiss"]
        )]
        reply: Option<String>,
        /// Dismiss the bead (write canonical note + remove the loom:* label).
        #[arg(long, short = 'd')]
        dismiss: bool,
        /// Launch an interactive Drafter chat session (I2, wx-2dreh).
        /// Renders the msg.md template and spawns a container with the
        /// claude backend attached to the user's terminal. Mutually
        /// exclusive with `-o`, `-r`, `-d`, `-b`, `-n` — the chat
        /// session walks every outstanding clarify, no single-bead
        /// selection. `-s <label>` may scope the walk to one spec.
        #[arg(
            long,
            short = 'c',
            conflicts_with_all = ["option", "reply", "dismiss", "bead", "number"]
        )]
        chat: bool,
    },
    /// Decompose the active spec into beads (four-tier detection).
    #[command(next_help_heading = "Workflow")]
    Todo {
        /// Spec label override (defaults to `current_spec`).
        #[arg(long, short = 's', value_name = "LABEL")]
        spec: Option<String>,
        /// Override the anchor's `base_commit` for tier-1 detection.
        #[arg(long, value_name = "COMMIT")]
        since: Option<String>,
    },
    /// Manage SQLite-backed notes for a spec — replacement for the
    /// deprecated `## Implementation Notes` markdown path (D2,
    /// wx-b1f1p).
    #[command(next_help_heading = "Workspace")]
    Note {
        #[command(subcommand)]
        action: NoteAction,
    },
    /// Audit spec criteria against test dispatchers (`[verify]` → stub-or-real).
    #[command(next_help_heading = "Inspect")]
    Doctor {
        /// Which audit to run. Currently only `criteria` (stub-or-real
        /// checks for `[verify](tests/loom-test.sh::test_X)` annotations).
        #[arg(long, default_value = "criteria")]
        check: String,
        /// Promote warning-severity findings (e.g. orphan stubs) to
        /// errors. Hard errors (stub-checked, missing dispatcher) always
        /// fail regardless.
        #[arg(long)]
        strict: bool,
    },
}

impl Command {
    /// `true` when this subcommand spawns containers or mutates workspace
    /// state — those are refused under `LOOM_INSIDE=1` to prevent a nested
    /// driver. Read-only subcommands (`status`, `logs`, `spec`) return
    /// `false`. Spec: `loom-harness.md` § Nested-Loom Guard.
    fn refused_inside_loom(&self) -> bool {
        match self {
            Command::Status
            | Command::Logs { .. }
            | Command::Spec { .. }
            | Command::Doctor { .. } => false,
            Command::Init { .. }
            | Command::UseSpec { .. }
            | Command::Plan { .. }
            | Command::Run { .. }
            | Command::Check { .. }
            | Command::Msg { .. }
            | Command::Note { .. }
            | Command::Todo { .. } => true,
        }
    }
}

fn main() -> ExitCode {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let cli = Cli::parse();

    if std::env::var_os(LOOM_INSIDE_ENV).is_some() && cli.command.refused_inside_loom() {
        eprintln!(
            "error: loom cannot run inside a loom-managed container\n  this command spawns containers or mutates workspace state, which\n  would create a nested driver. read-only commands (status, logs,\n  spec) are still available."
        );
        return ExitCode::from(2);
    }

    let workspace = cli
        .workspace
        .unwrap_or_else(|| match std::env::current_dir() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("loom: failed to read current dir: {e}");
                std::process::exit(2);
            }
        });

    let agent_override = cli.agent.map(AgentKind::from);

    let result = match cli.command {
        Command::Init { rebuild } => run_init(&workspace, rebuild),
        Command::Status => run_status(&workspace),
        Command::UseSpec { label } => run_use(&workspace, &label),
        Command::Logs { bead } => run_logs(&workspace, bead.as_deref()),
        Command::Spec { deps } => run_spec(&workspace, deps),
        Command::Plan {
            new,
            update,
            profile,
        } => run_plan(&workspace, new, update, profile),
        Command::Run {
            once,
            parallel,
            profile,
            spec,
        } => run_run(&workspace, once, parallel, profile, spec, agent_override),
        Command::Check { spec } => run_check(&workspace, spec, agent_override),
        Command::Msg {
            spec,
            number,
            bead,
            option,
            reply,
            dismiss,
            chat,
        } => run_msg(&workspace, spec, number, bead, option, reply, dismiss, chat),
        Command::Todo { spec, since } => run_todo(&workspace, spec, since, agent_override),
        Command::Note { action } => run_note(&workspace, action),
        Command::Doctor { check, strict } => run_doctor(&workspace, &check, strict),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("loom: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn run_init(workspace: &std::path::Path, rebuild: bool) -> anyhow::Result<()> {
    let molecules = if rebuild {
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(async {
            let bd = BdClient::new();
            init::fetch_active_molecules(&bd).await
        })?
    } else {
        Vec::new()
    };
    let report = init::run(workspace, init::InitOpts { rebuild }, &molecules)?;
    println!("loom init: workspace={}", workspace.display());
    println!(
        "  config: {} ({})",
        report.config_path.display(),
        if report.config_created {
            "created"
        } else {
            "kept existing"
        }
    );
    println!("  state.db: {}", report.state_db_path.display());
    if let Some(rb) = report.rebuild {
        println!(
            "  rebuilt {} spec(s), {} molecule(s), {} companion(s)",
            rb.specs, rb.molecules, rb.companions,
        );
    }
    Ok(())
}

fn run_note(workspace: &std::path::Path, action: NoteAction) -> anyhow::Result<()> {
    let db = loom_driver::state::StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let clock = SystemClock::new();
    let now_ms = clock
        .wall_now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    match action {
        NoteAction::Set { label, json, kind } => {
            let label = SpecLabel::new(&label);
            let notes: Vec<String> = serde_json::from_str(&json)
                .map_err(|e| anyhow::anyhow!("--json must be a JSON array of strings: {e}"))?;
            db.notes_set(&label, &kind, &notes, now_ms)?;
            println!(
                "loom note set: replaced {} note(s) for spec {} (kind {kind})",
                notes.len(),
                label.as_str(),
            );
        }
        NoteAction::Add { label, text, kind } => {
            let label = SpecLabel::new(&label);
            let id = db.notes_add(&label, &kind, &text, now_ms)?;
            println!(
                "loom note add: id={id} spec={label} kind={kind}",
                label = label.as_str(),
            );
        }
        NoteAction::Clear {
            label,
            kind,
            all_kinds,
        } => {
            let label = SpecLabel::new(&label);
            let kind_arg = if all_kinds { None } else { Some(kind.as_str()) };
            db.notes_clear(&label, kind_arg)?;
            println!(
                "loom note clear: spec={} kind={}",
                label.as_str(),
                if all_kinds { "<all>" } else { kind.as_str() },
            );
        }
        NoteAction::List {
            label,
            kind,
            all_kinds,
        } => {
            let label_obj = label.as_deref().map(SpecLabel::new);
            let kind_arg = if all_kinds { None } else { Some(kind.as_str()) };
            let rows = db.notes_list(label_obj.as_ref(), kind_arg)?;
            for row in rows {
                if all_kinds {
                    println!(
                        "{id:>5} [{spec}/{kind}] {text}",
                        id = row.id,
                        spec = row.spec_label,
                        kind = row.kind,
                        text = row.text,
                    );
                } else {
                    println!(
                        "{id:>5} [{spec}] {text}",
                        id = row.id,
                        spec = row.spec_label,
                        text = row.text,
                    );
                }
            }
        }
        NoteAction::Rm { id } => {
            db.notes_rm(id)?;
            println!("loom note rm: removed id={id}");
        }
    }
    Ok(())
}

fn run_doctor(workspace: &std::path::Path, check: &str, strict: bool) -> anyhow::Result<()> {
    if check != "criteria" {
        anyhow::bail!("unsupported --check value `{check}` (only `criteria` is implemented)");
    }
    let specs_dir = workspace.join("specs");
    let dispatcher = workspace.join("tests/loom-test.sh");
    let findings = doctor::audit(&specs_dir, &dispatcher)?;
    let code = doctor::report(&findings, strict);
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

fn run_status(workspace: &std::path::Path) -> anyhow::Result<()> {
    let db = loom_driver::state::StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let report = status::load(&db)?;
    print!("{}", status::render(&report));
    Ok(())
}

fn run_use(workspace: &std::path::Path, label: &str) -> anyhow::Result<()> {
    let label = SpecLabel::new(label);
    let db_path = workspace.join(".wrapix/loom/state.db");
    use_spec::run(workspace, &label, &db_path)?;
    println!("active spec: {label}");
    Ok(())
}

fn run_logs(workspace: &std::path::Path, bead: Option<&str>) -> anyhow::Result<()> {
    let logs_root = workspace.join(".wrapix/loom/logs");
    let bead_id = bead.map(BeadId::new).transpose()?;
    let path = logs_cmd::select_log(
        &logs_root,
        logs_cmd::LogsOpts {
            bead: bead_id.as_ref(),
        },
    )?;
    println!("{}", path.display());
    Ok(())
}

fn run_plan(
    workspace: &std::path::Path,
    new: Option<String>,
    update: Option<String>,
    profile: Option<String>,
) -> anyhow::Result<()> {
    let manifest = ProfileImageManifest::from_env()?;
    let mode = plan::parse_mode(new, update)?;
    let report = plan::run(
        workspace,
        plan::PlanOpts {
            mode,
            wrapix_bin: std::env::var_os("LOOM_WRAPIX_BIN").map(PathBuf::from),
            cli_profile: profile.map(ProfileName::new),
            manifest,
        },
    )?;
    println!("loom plan: spec={}", report.spec_path.display());
    if report.companion_paths.is_empty() {
        if report.companions_section_present {
            println!("  companions: (none)");
        } else {
            println!("  companions: (none — interview did not declare companions)");
        }
    } else {
        println!("  companions:");
        for path in &report.companion_paths {
            println!("    - {path}");
        }
    }
    Ok(())
}

fn run_run(
    workspace: &Path,
    once: bool,
    parallel: Parallelism,
    profile: Option<String>,
    spec: Option<String>,
    agent_override: Option<AgentKind>,
) -> anyhow::Result<()> {
    let manifest = Arc::new(ProfileImageManifest::from_env()?);
    let label = resolve_spec_label(workspace, spec)?;
    let lock_mgr = LockManager::new(workspace)?;
    let guard = lock_mgr.acquire_spec(&label)?;

    let config = LoomConfig::load(workspace.join(".wrapix/loom/config.toml"))?;
    sweep_retention_at(
        &workspace.join(".wrapix/loom/logs"),
        config.logs.retention_days,
        SystemClock::new().wall_now(),
    );
    // Resolve the per-phase backend up front so an unknown backend name in
    // the config (or via `--agent` — clap covers the latter) fails before
    // any work begins. The resolution itself is the wiring; the dispatch
    // closure handed to the parallel batch driver below is what consumes it.
    let selection = resolved_agent_for(&config, agent_override, Phase::Run)?;
    let phase_default = selection.profile.clone();
    let cli_profile = profile.map(ProfileName::new);

    let loom_bin = current_loom_bin()?;
    let runtime = tokio::runtime::Runtime::new()?;

    if !parallel.is_one() {
        let parallel_n = parallel.get();
        let workspace_buf = workspace.to_path_buf();
        let label_for_async = label.clone();
        let manifest_for_async = Arc::clone(&manifest);
        let cli_profile_for_async = cli_profile.clone();
        let phase_default_for_async = phase_default.clone();
        let kind = selection.kind;
        let shutdown_grace = resolve_shutdown_grace(&selection);
        let summary = runtime.block_on(async move {
            run_parallel_run(
                workspace_buf,
                label_for_async,
                parallel_n,
                kind,
                shutdown_grace,
                manifest_for_async,
                cli_profile_for_async,
                phase_default_for_async,
            )
            .await
        })?;
        println!(
            "loom run --parallel {parallel_n}: merged {}, conflicted {}, failed {}",
            summary.merged, summary.conflicted, summary.failed,
        );
        return Ok(());
    }

    let mode = if once {
        RunMode::Once
    } else {
        RunMode::Continuous
    };
    let manifest_for_seq = Arc::clone(&manifest);
    let kind = selection.kind;
    let shutdown_grace = resolve_shutdown_grace(&selection);
    let workspace_buf = workspace.to_path_buf();
    let logs_root = workspace.join(".wrapix/loom/logs");
    let label_for_sink = label.clone();
    let summary = runtime.block_on(async move {
        let bd = BdClient::new();
        let mut controller = ProductionAgentLoopController::new(
            bd,
            label.clone(),
            loom_bin,
            workspace_buf,
            manifest_for_seq,
            cli_profile,
            phase_default,
            move |spawn_cfg: SpawnConfig, bead_id: BeadId| {
                let logs_root = logs_root.clone();
                let label = label_for_sink.clone();
                async move {
                    // A sink-open failure is pre-spawn — the bead's
                    // JSONL log location is part of the workflow's
                    // pre-flight setup. Bubble it through the same
                    // `infra-preflight` path so `bd update` records the
                    // cause instead of the error tearing down `loom run`.
                    let sink = match open_bead_sink(&logs_root, &label, &bead_id) {
                        Ok(s) => Some(s),
                        Err(err) => {
                            return (
                                SessionResult::PreflightFailed {
                                    error: format!("open log sink: {err}"),
                                },
                                None,
                            );
                        }
                    };
                    let mut output = String::new();
                    let session = dispatch_classified(
                        kind,
                        spawn_cfg,
                        shutdown_grace,
                        sink,
                        Some(&mut output),
                    )
                    .await;
                    let marker = parse_exit_signal(&output);
                    (session, marker)
                }
            },
        )
        .with_handoff_lock(guard);
        run_loop(&mut controller, mode, RetryPolicy::default()).await
    })?;
    println!(
        "loom run: processed {} bead(s), clarified {}, blocked {}, molecule_complete={}, \
         execed_check={}",
        summary.beads_processed,
        summary.beads_clarified,
        summary.beads_blocked,
        summary.molecule_complete,
        summary.execed_check,
    );
    Ok(())
}

/// Aggregate counts surfaced from `loom run --parallel N` for the human
/// summary line.
struct ParallelRunSummary {
    merged: usize,
    conflicted: usize,
    failed: usize,
}

async fn run_parallel_run(
    workspace: PathBuf,
    label: SpecLabel,
    parallel_n: u32,
    kind: AgentKind,
    shutdown_grace: Option<Duration>,
    manifest: Arc<ProfileImageManifest>,
    cli_profile: Option<ProfileName>,
    phase_default: ProfileName,
) -> anyhow::Result<ParallelRunSummary> {
    use loom_driver::bd::UpdateOpts;
    use loom_workflow::run::{AgentOutcome, run_parallel_batch};

    let bd = BdClient::new();
    let beads = bd
        .ready(loom_driver::bd::ReadyOpts {
            limit: Some(parallel_n),
            label: Some(format!("spec:{}", label.as_str())),
        })
        .await?;
    if beads.is_empty() {
        return Ok(ParallelRunSummary {
            merged: 0,
            conflicted: 0,
            failed: 0,
        });
    }

    let git = GitClient::open(workspace.clone())?;
    let logs_root = workspace.join(".wrapix/loom/logs");
    let label_for_closure = label.clone();
    let outcome = run_parallel_batch(&git, &label, beads, move |slot| {
        let manifest_inner = Arc::clone(&manifest);
        let cli_profile_inner = cli_profile.clone();
        let phase_default_inner = phase_default.clone();
        let logs_root_inner = logs_root.clone();
        let label_inner = label_for_closure.clone();
        async move {
            // Marker is the primary signal here too — without it, parallel
            // mode would swallow `LOOM_BLOCKED` / `LOOM_CLARIFY` self-reports
            // the same way the sequential path used to.
            match dispatch_for_slot(
                kind,
                shutdown_grace,
                slot,
                &manifest_inner,
                cli_profile_inner.as_ref(),
                &phase_default_inner,
                &logs_root_inner,
                &label_inner,
            )
            .await
            {
                Ok((session, marker)) => match (marker, session.exit_code) {
                    (Some(ExitSignal::Blocked { reason }), _) => AgentOutcome::Blocked { reason },
                    (Some(ExitSignal::Clarify { question }), _) => {
                        AgentOutcome::Clarify { question }
                    }
                    (Some(ExitSignal::Complete | ExitSignal::Noop), 0) => AgentOutcome::Success,
                    (Some(ExitSignal::Complete | ExitSignal::Noop), code) => {
                        AgentOutcome::Failure {
                            error: format!("agent emitted COMPLETE/NOOP but exited code {code}"),
                        }
                    }
                    (None, 0) => AgentOutcome::Failure {
                        error: "agent exited 0 without LOOM_* marker (swallowed marker)"
                            .to_string(),
                    },
                    (None, code) => AgentOutcome::Failure {
                        error: format!("agent exited with code {code}"),
                    },
                },
                Err(e) => AgentOutcome::Failure {
                    error: format!("{e}"),
                },
            }
        }
    })
    .await?;

    // Apply labels for marker self-reports. The bd-side cleanup mirrors the
    // sequential path's `apply_clarify` / `apply_blocked` so a clarify in
    // parallel mode is indistinguishable from one in sequential mode.
    let bd_label = BdClient::new();
    for (bead, question) in outcome.clarified() {
        let notes = if question.is_empty() {
            None
        } else {
            Some(question)
        };
        bd_label
            .update(
                &bead,
                UpdateOpts {
                    add_labels: vec!["loom:clarify".to_string()],
                    notes,
                    ..UpdateOpts::default()
                },
            )
            .await?;
    }
    for (bead, reason) in outcome.blocked() {
        let notes = if reason.is_empty() {
            "agent-blocked".to_string()
        } else {
            format!("agent-blocked: {reason}")
        };
        bd_label
            .update(
                &bead,
                UpdateOpts {
                    add_labels: vec!["loom:blocked".to_string()],
                    notes: Some(notes),
                    ..UpdateOpts::default()
                },
            )
            .await?;
    }

    Ok(ParallelRunSummary {
        merged: outcome.merged_ids().len(),
        conflicted: outcome.conflict_ids().len(),
        failed: outcome.failure_ids().len(),
    })
}

/// One slot's dispatch: build the per-bead [`SpawnConfig`] against the
/// slot's worktree and hand it to the same [`dispatch`] match the sequential
/// path uses. The pre-resolved [`AgentKind`] from `run_run` is threaded down
/// — this used to reload `LoomConfig` and re-resolve the backend per slot,
/// which let the sequential and parallel paths drift if the on-disk config
/// changed mid-run. A missing manifest entry surfaces as
/// [`ProfileError::UnknownProfile`] (via `RunError::Profile`) so the caller
/// converts it to a typed [`AgentOutcome::Failure`] without falling back to
/// a silent default.
///
/// [`ProfileError::UnknownProfile`]: loom_driver::profile_manifest::ProfileError::UnknownProfile
async fn dispatch_for_slot(
    kind: AgentKind,
    shutdown_grace: Option<Duration>,
    slot: loom_workflow::run::WorktreeBead,
    manifest: &ProfileImageManifest,
    cli_profile: Option<&ProfileName>,
    phase_default: &ProfileName,
    logs_root: &Path,
    label: &SpecLabel,
) -> anyhow::Result<(SessionOutcome, Option<ExitSignal>)> {
    use loom_driver::scratch::ScratchSession;
    use loom_workflow::run::{
        RunContextInputs, build_spawn_config_from_manifest, render_run_prompt,
    };

    let banner = format!("loom run @ {}", slot.bead.id);
    let key = resolve_scratch_key(Phase::Run, label, Some(&slot.bead.id));
    let scratchpad_path = ScratchSession::scratchpad_path_for(&slot.worktree.path, &key)
        .to_string_lossy()
        .into_owned();
    let initial_prompt = render_run_prompt(RunContextInputs {
        label: label.clone(),
        spec_path: format!("specs/{}.md", label.as_str()),
        pinned_context: String::new(),
        companion_paths: vec![],
        molecule_id: None,
        issue_id: slot.bead.id.clone(),
        title: slot.bead.title.clone(),
        description: slot.bead.description.clone(),
        previous_failure: None,
        scratchpad_path,
        exit_signals: String::new(),
    })?;
    let scratch = ScratchSession::open(&slot.worktree.path, &key, &initial_prompt, &banner)?;
    let spawn_config = build_spawn_config_from_manifest(
        manifest,
        &slot.bead,
        cli_profile,
        phase_default,
        slot.worktree.path.clone(),
        initial_prompt,
        scratch.path().to_path_buf(),
        vec![],
        vec![],
    )?;

    let sink = open_bead_sink(logs_root, label, &slot.bead.id)?;
    let mut output = String::new();
    let result = dispatch(
        kind,
        spawn_config,
        shutdown_grace,
        Some(sink),
        Some(&mut output),
    )
    .await;
    drop(scratch);
    let outcome = result?;
    let marker = parse_exit_signal(&output);
    Ok((outcome, marker))
}

/// Backend-agnostic dispatcher. The match is the only place in the binary
/// that knows the concrete backend types — `run_agent` is monomorphized once
/// per arm at compile time, so the workflow modules never see them.
///
/// `sink` is consumed: ownership crosses into [`run_agent`], which finishes
/// it before returning. Phase entry points open the sink before invoking
/// dispatch so the on-disk JSONL and the workflow outcome share one code
/// path. Pass `None` from sites that have not yet been wired.
///
/// `shutdown_grace` is the configured `[claude] post_result_grace_secs`
/// resolved from [`AgentSelection::claude_settings`]. It is patched into
/// `spawn.shutdown_grace` only when the dispatched backend is claude and
/// the field is not already set — pi exits naturally on `agent_end`, and
/// upstream callers that pre-populate the field (tests, future per-bead
/// overrides) are honored as-is.
async fn dispatch(
    kind: AgentKind,
    mut spawn: SpawnConfig,
    shutdown_grace: Option<Duration>,
    sink: Option<LogSink>,
    text_capture: Option<&mut String>,
) -> Result<SessionOutcome, ProtocolError> {
    if matches!(kind, AgentKind::Claude) && spawn.shutdown_grace.is_none() {
        spawn.shutdown_grace = shutdown_grace;
    }
    if spawn.handshake_timeout.is_none()
        && let Some(d) = duration_env_ms("LOOM_HANDSHAKE_TIMEOUT_MS")
    {
        spawn.handshake_timeout = Some(d);
    }
    if spawn.stall_warn_interval.is_none()
        && let Some(d) = duration_env_ms("LOOM_STALL_WARN_MS")
    {
        spawn.stall_warn_interval = Some(d);
    }
    match kind {
        AgentKind::Pi => run_agent::<PiBackend>(&spawn, sink, text_capture).await,
        AgentKind::Claude => run_agent::<ClaudeBackend>(&spawn, sink, text_capture).await,
    }
}

/// Same as [`dispatch`] but preserves the preflight-vs-mid-session split via
/// [`SessionResult`]. The `loom run` driver consumes this so the verdict gate
/// can route preflight failures to `infra-preflight` immediately and grant
/// mid-session failures one driver-memory retry per `loom run`.
async fn dispatch_classified(
    kind: AgentKind,
    mut spawn: SpawnConfig,
    shutdown_grace: Option<Duration>,
    sink: Option<LogSink>,
    text_capture: Option<&mut String>,
) -> SessionResult {
    if matches!(kind, AgentKind::Claude) && spawn.shutdown_grace.is_none() {
        spawn.shutdown_grace = shutdown_grace;
    }
    if spawn.handshake_timeout.is_none()
        && let Some(d) = duration_env_ms("LOOM_HANDSHAKE_TIMEOUT_MS")
    {
        spawn.handshake_timeout = Some(d);
    }
    if spawn.stall_warn_interval.is_none()
        && let Some(d) = duration_env_ms("LOOM_STALL_WARN_MS")
    {
        spawn.stall_warn_interval = Some(d);
    }
    match kind {
        AgentKind::Pi => run_agent_classified::<PiBackend>(&spawn, sink, text_capture).await,
        AgentKind::Claude => {
            run_agent_classified::<ClaudeBackend>(&spawn, sink, text_capture).await
        }
    }
}

/// Test seam: read a millisecond budget from `name` if set. Production
/// runs leave the env vars unset and SpawnConfig falls back to the
/// constants in `loom_driver::agent` (30s handshake / 60s stall warn).
fn duration_env_ms(name: &str) -> Option<Duration> {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis)
}

/// Resolve the configured shutdown grace from the active agent selection.
/// Pi sessions return `None` because pi exits naturally on `agent_end`;
/// claude sessions return the parsed `[claude] post_result_grace_secs`.
fn resolve_shutdown_grace(selection: &loom_driver::config::AgentSelection) -> Option<Duration> {
    selection
        .claude_settings
        .as_ref()
        .map(|s| Duration::from_secs(u64::from(s.post_result_grace_secs)))
}

/// Open the per-bead JSONL sink at the path the spec promises:
/// `<logs_root>/<spec>/<bead-id>-<utc>.jsonl`. Renderer is `None` because
/// the sequential and parallel run dispatchers run non-interactively (the
/// human-facing summary is written by the `loom run` outer-loop print).
fn open_bead_sink(
    logs_root: &Path,
    label: &SpecLabel,
    bead_id: &BeadId,
) -> Result<LogSink, ProtocolError> {
    LogSink::open_in_at(
        logs_root,
        label,
        bead_id,
        None,
        SystemClock::new().wall_now(),
    )
    .map_err(|e| ProtocolError::Io(std::io::Error::other(e.to_string())))
}

/// Resolve `phase`'s [`AgentKind`] honoring the global `--agent` override.
/// CLI override wins over `[phase.<phase>] agent.backend` and
/// `[phase.default] agent.backend`. Returns the full [`AgentSelection`] so
/// callers retain access to profile / provider / model / claude_settings.
fn resolved_agent_for(
    config: &LoomConfig,
    agent_override: Option<AgentKind>,
    phase: Phase,
) -> anyhow::Result<loom_driver::config::AgentSelection> {
    let mut selection = config.agent_for(phase)?;
    if let Some(kind) = agent_override {
        selection.kind = kind;
        selection.claude_settings = match kind {
            AgentKind::Claude => Some(loom_driver::config::ClaudeSettings {
                denied_tools: config.security.denied_tools.clone(),
                post_result_grace_secs: config.claude.post_result_grace_secs,
            }),
            AgentKind::Pi => None,
        };
    }
    Ok(selection)
}

fn run_check(
    workspace: &Path,
    spec: Option<String>,
    agent_override: Option<AgentKind>,
) -> anyhow::Result<()> {
    let manifest = Arc::new(ProfileImageManifest::from_env()?);
    let label = resolve_spec_label(workspace, spec)?;
    let lock_mgr = LockManager::new(workspace)?;
    let guard = lock_mgr.acquire_spec(&label)?;

    let config = LoomConfig::load(workspace.join(".wrapix/loom/config.toml"))?;
    let selection = resolved_agent_for(&config, agent_override, Phase::Check)?;
    let phase_default = selection.profile.clone();
    let kind = selection.kind;
    let shutdown_grace = resolve_shutdown_grace(&selection);

    let loom_bin = current_loom_bin()?;
    let state = std::sync::Arc::new(StateDb::open(workspace.join(".wrapix/loom/state.db"))?);
    let runtime = tokio::runtime::Runtime::new()?;
    let workspace_buf = workspace.to_path_buf();
    let logs_root = workspace.join(".wrapix/loom/logs");
    let label_for_sink = label.clone();
    let result = runtime.block_on(async move {
        let bd = BdClient::new();
        let mut controller = ProductionCheckController::new(
            bd,
            label.clone(),
            loom_bin,
            workspace_buf,
            state,
            manifest,
            phase_default,
            move |spawn_cfg: SpawnConfig| {
                let logs_root = logs_root.clone();
                let label = label_for_sink.clone();
                async move {
                    let sink = LogSink::open_phase_at(
                        &logs_root,
                        &label,
                        "check",
                        None,
                        SystemClock::new().wall_now(),
                    )
                    .map_err(|e| ProtocolError::Io(std::io::Error::other(e.to_string())))?;
                    dispatch(kind, spawn_cfg, shutdown_grace, Some(sink), None).await
                }
            },
        )
        .with_handoff_lock(guard);
        run_check_loop(&mut controller, IterationCap::default()).await
    })?;
    println!("loom check: {result:?}");
    Ok(())
}

fn run_msg(
    workspace: &Path,
    spec: Option<String>,
    number: Option<u32>,
    bead: Option<String>,
    option: Option<u32>,
    reply: Option<String>,
    dismiss: bool,
    chat: bool,
) -> anyhow::Result<()> {
    let _manifest = ProfileImageManifest::from_env()?;
    let spec_filter = spec.as_deref().map(SpecLabel::new);
    if chat {
        return run_msg_chat(workspace, spec_filter);
    }
    if let Some(label) = &spec_filter {
        let lock_mgr = LockManager::new(workspace)?;
        let _guard = lock_mgr.acquire_spec(label)?;
        run_msg_inner(number, bead, option, reply, dismiss, spec_filter)
    } else {
        run_msg_inner(number, bead, option, reply, dismiss, None)
    }
}

/// `loom msg -c [-s <label>]` — interactive Drafter chat session.
///
/// Walks every outstanding clarify/blocked bead under the optional
/// spec filter and hands them to an interactive claude session via
/// the wrapix sandbox + msg.md template. The session writes resolution
/// notes via `bd update --notes` and clears the label via
/// `bd update --remove-label` per resolved bead.
///
/// **Status:** scaffold only. The chat flag is recognized and the
/// command branches into this function, but the wrapix-run /
/// claude-attach plumbing is a focused follow-up — needs PTY
/// passthrough, signal handling, and the msg.md template wired
/// against the same controller surface `loom run` uses. The
/// non-interactive `loom msg -o/-r/-d` paths (B1, I1) cover the
/// programmatic case in the meantime.
fn run_msg_chat(workspace: &Path, spec_filter: Option<SpecLabel>) -> anyhow::Result<()> {
    let scope = spec_filter
        .as_ref()
        .map(|l| format!(" filtered to spec:{}", l.as_str()))
        .unwrap_or_default();
    println!(
        "loom msg --chat: interactive Drafter session{scope} — not yet implemented.\n\
         The chat session would render the msg.md template against the outstanding\n\
         clarify/blocked beads and spawn a wrapix container running claude attached\n\
         to this terminal. Resolution notes flow back via `bd update --notes` and\n\
         the loom:* labels clear on confirmation.\n\
         In the meantime, use `loom msg -o <N> -b <id>` for option fast-reply or\n\
         `loom msg -r \"<text>\" -b <id>` for verbatim reply.\n\
         Workspace: {workspace}",
        workspace = workspace.display(),
    );
    Ok(())
}

fn run_msg_inner(
    number: Option<u32>,
    bead: Option<String>,
    option: Option<u32>,
    reply: Option<String>,
    dismiss: bool,
    spec_filter: Option<SpecLabel>,
) -> anyhow::Result<()> {
    let has_action = option.is_some() || reply.is_some() || dismiss;

    let runtime = tokio::runtime::Runtime::new()?;
    let beads = runtime.block_on(async {
        let bd = BdClient::new();
        bd.list(ListOpts {
            status: None,
            label: None,
            label_any: vec!["loom:clarify".to_string(), "loom:blocked".to_string()],
        })
        .await
    })?;
    let kept = filter_msg_beads(&beads, spec_filter.as_ref());

    if !has_action {
        let rows = build_rows(&kept, spec_filter.as_ref());
        if rows.is_empty() {
            println!("(no outstanding clarify or blocked beads)");
            return Ok(());
        }
        for row in rows {
            match row.spec {
                Some(s) => println!(
                    "{:>3}. {} [{}] [spec:{}] {}",
                    row.index,
                    row.bead_id,
                    row.kind.tag(),
                    s,
                    row.summary
                ),
                None => println!(
                    "{:>3}. {} [{}] {}",
                    row.index,
                    row.bead_id,
                    row.kind.tag(),
                    row.summary
                ),
            }
        }
        return Ok(());
    }

    let (target, _pos) = resolve_target(&kept, number, bead.as_deref())?;
    let target_bead = kept
        .iter()
        .find(|b| b.id == target)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("bead {target} not in filtered list"))?;
    let kind = kind_of(target_bead).ok_or_else(|| {
        anyhow::anyhow!("bead {target} carries neither loom:clarify nor loom:blocked")
    })?;
    let label_to_remove = kind.label().to_string();

    if let Some(opt_idx) = option {
        // `-o <int>` strict option lookup: parse the bead's description,
        // require `### Option <int>` to exist, compose the canonical
        // `"Chose option N — title: body"` note. Validation runs before
        // any bd state mutation per the I1 acceptance.
        let note = compose_option_note(&target, opt_idx, &target_bead.description)?;
        let runtime = tokio::runtime::Runtime::new()?;
        let id_clone = target.clone();
        let note_for_bd = note.clone();
        runtime.block_on(async move {
            let bd = BdClient::new();
            bd.update(
                &id_clone,
                UpdateOpts {
                    remove_labels: vec![label_to_remove],
                    notes: Some(note_for_bd),
                    ..UpdateOpts::default()
                },
            )
            .await
        })?;
        println!("answered {target}: {note}");
        if let Some(label) = spec_label_of(target_bead) {
            println!("resume: loom run -s {label}");
        }
        return Ok(());
    }

    if let Some(text) = reply {
        // `-r <text>` verbatim: store the raw text on the bead, drop the
        // loom:* label. Works on any bead kind regardless of Options.
        let runtime = tokio::runtime::Runtime::new()?;
        let id_clone = target.clone();
        let text_for_bd = text.clone();
        runtime.block_on(async move {
            let bd = BdClient::new();
            bd.update(
                &id_clone,
                UpdateOpts {
                    remove_labels: vec![label_to_remove],
                    notes: Some(text_for_bd),
                    ..UpdateOpts::default()
                },
            )
            .await
        })?;
        println!("answered {target}: {text}");
        if let Some(label) = spec_label_of(target_bead) {
            println!("resume: loom run -s {label}");
        }
        return Ok(());
    }

    if dismiss {
        let runtime = tokio::runtime::Runtime::new()?;
        let id_clone = target.clone();
        runtime.block_on(async move {
            let bd = BdClient::new();
            bd.update(
                &id_clone,
                UpdateOpts {
                    remove_labels: vec![label_to_remove],
                    notes: Some(DISMISS_NOTE.to_string()),
                    ..UpdateOpts::default()
                },
            )
            .await
        })?;
        println!("dismissed {target}: {DISMISS_NOTE}");
        if let Some(label) = spec_label_of(target_bead) {
            println!("resume: loom run -s {label}");
        }
    }
    Ok(())
}

fn run_todo(
    workspace: &Path,
    spec: Option<String>,
    since: Option<String>,
    agent_override: Option<AgentKind>,
) -> anyhow::Result<()> {
    let manifest = Arc::new(ProfileImageManifest::from_env()?);
    let label = resolve_spec_label(workspace, spec)?;
    let lock_mgr = LockManager::new(workspace)?;
    let _guard = lock_mgr.acquire_spec(&label)?;

    let config = LoomConfig::load(workspace.join(".wrapix/loom/config.toml"))?;
    let selection = resolved_agent_for(&config, agent_override, Phase::Todo)?;
    let phase_default = selection.profile.clone();
    let kind = selection.kind;
    let shutdown_grace = resolve_shutdown_grace(&selection);

    let state = Arc::new(StateDb::open(workspace.join(".wrapix/loom/state.db"))?);
    let git = Arc::new(GitClient::open(workspace)?);
    let runtime = tokio::runtime::Runtime::new()?;
    let workspace_buf = workspace.to_path_buf();
    let logs_root = workspace.join(".wrapix/loom/logs");
    let label_for_sink = label.clone();
    let summary = runtime.block_on(async move {
        let mut controller = ProductionTodoController::new(
            label,
            workspace_buf,
            state,
            manifest,
            phase_default,
            git,
            since,
        );
        run_todo_workflow(&mut controller, |spawn_cfg: SpawnConfig| async move {
            let sink = LogSink::open_phase_at(
                &logs_root,
                &label_for_sink,
                "todo",
                None,
                SystemClock::new().wall_now(),
            )
            .map_err(|e| ProtocolError::Io(std::io::Error::other(e.to_string())))?;
            let mut output = String::new();
            let outcome = dispatch(
                kind,
                spawn_cfg,
                shutdown_grace,
                Some(sink),
                Some(&mut output),
            )
            .await?;
            let marker = parse_exit_signal(&output);
            Ok((outcome, marker))
        })
        .await
    })?;
    println!(
        "loom todo: agent exited {}, cost_usd={:?}",
        summary.exit_code, summary.cost_usd
    );
    Ok(())
}

fn resolve_spec_label(workspace: &Path, spec: Option<String>) -> anyhow::Result<SpecLabel> {
    if let Some(s) = spec {
        return Ok(SpecLabel::new(s));
    }
    let db = StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    db.current_spec()?.ok_or_else(|| {
        anyhow::anyhow!("no active spec — pass -s <label> or run `loom use <label>`")
    })
}

fn current_loom_bin() -> anyhow::Result<PathBuf> {
    if let Some(bin) = std::env::var_os("LOOM_BIN") {
        return Ok(PathBuf::from(bin));
    }
    Ok(std::env::current_exe()?)
}

fn run_spec(workspace: &std::path::Path, deps: bool) -> anyhow::Result<()> {
    let db = loom_driver::state::StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let label = db
        .current_spec()?
        .ok_or_else(|| anyhow::anyhow!("no active spec — run `loom use <label>`"))?;
    if deps {
        let pkgs = spec::deps_for_label(workspace, &label)?;
        for pkg in pkgs {
            println!("{pkg}");
        }
    } else {
        let rows = spec::list_for_label(workspace, &label)?;
        for row in rows {
            let kind = match row.kind {
                spec::AnnotationKind::Verify => "verify",
                spec::AnnotationKind::Judge => "judge",
                spec::AnnotationKind::None => "none",
            };
            let file = row
                .file
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let function = row.function.unwrap_or_default();
            let checked = if row.checked { "x" } else { " " };
            println!(
                "[{checked}] {kind}\t{file}\t{function}\t{criterion}",
                criterion = row.criterion
            );
        }
    }
    Ok(())
}
