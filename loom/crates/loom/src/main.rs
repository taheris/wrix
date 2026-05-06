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
use loom_core::agent::{AgentKind, ProtocolError, SessionOutcome, SpawnConfig};
use loom_core::bd::{BdClient, ListOpts, UpdateOpts};
use loom_core::clock::{Clock, SystemClock};
use loom_core::config::{LoomConfig, Phase};
use loom_core::git::GitClient;
use loom_core::identifier::{BeadId, ProfileName, SpecLabel};
use loom_core::lock::LockManager;
use loom_core::logging::{LogSink, sweep_retention_at};
use loom_core::profile_manifest::ProfileImageManifest;
use loom_core::state::StateDb;
use loom_workflow::check::{IterationCap, ProductionCheckController, check_loop as run_check_loop};
use loom_workflow::msg::{
    DISMISS_NOTE, FastReply, build_fast_reply, build_rows, filter_clarifies, resolve_target,
    spec_label_of,
};
use loom_workflow::run::{
    Parallelism, ProductionAgentLoopController, RetryPolicy, RunMode, SessionResult, run_loop,
};
use loom_workflow::todo::{ProductionTodoController, parse_exit_signal, run as run_todo_workflow};
use loom_workflow::{init, logs_cmd, plan, spec, status, use_spec};
use loom_workflow::{run_agent, run_agent_classified};

/// Top-level CLI surface.
#[derive(Debug, Parser)]
#[command(name = "loom", version, about = "Loom harness CLI")]
struct Cli {
    /// Workspace root. Defaults to the current working directory.
    #[arg(long, global = true, value_name = "PATH")]
    workspace: Option<PathBuf>,

    /// Override the agent backend for this invocation. Wins over per-phase
    /// `[phase.<phase>] agent.backend = ...` and `[phase.default]
    /// agent.backend = ...` in `.wrapix/loom/config.toml`. Accepts `claude`
    /// or `pi`; any other value triggers a clap parse error.
    #[arg(long, global = true, value_enum, value_name = "BACKEND")]
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

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize the workspace (create `.wrapix/loom/` config + state DB).
    Init {
        /// Drop and repopulate the state DB from `specs/*.md` and active beads.
        #[arg(long)]
        rebuild: bool,
    },
    /// Print the active spec, current molecule, and iteration counter.
    Status,
    /// Set the active spec.
    #[command(name = "use")]
    UseSpec {
        /// Spec label (matches `<workspace>/specs/<label>.md`).
        label: String,
    },
    /// Tail the most recent per-bead JSONL log.
    Logs {
        /// Restrict the search to a specific bead id.
        #[arg(long)]
        bead: Option<String>,
    },
    /// Inspect spec annotations and tooling dependencies.
    Spec {
        /// Print the unique nixpkgs names referenced by verify/judge tests.
        #[arg(long)]
        deps: bool,
    },
    /// Interactive spec interview (`-n <label>` new, `-u <label>` update).
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
    Check {
        /// Spec label override (defaults to `current_spec`).
        #[arg(long, short = 's', value_name = "LABEL")]
        spec: Option<String>,
    },
    /// Resolve outstanding clarify beads.
    Msg {
        /// Filter to a specific spec label.
        #[arg(long, short = 's', value_name = "LABEL")]
        spec: Option<String>,
        /// Select clarify by 1-based index in the printed list.
        #[arg(short = 'n', value_name = "N")]
        index: Option<u32>,
        /// Select clarify by bead id.
        #[arg(short = 'i', value_name = "ID")]
        id: Option<String>,
        /// Fast-reply: integer chooses option N; anything else stored verbatim.
        #[arg(short = 'a', value_name = "CHOICE")]
        answer: Option<String>,
        /// Dismiss the clarify (write canonical note + remove the label).
        #[arg(short = 'd')]
        dismiss: bool,
    },
    /// Decompose the active spec into beads (four-tier detection).
    Todo {
        /// Spec label override (defaults to `current_spec`).
        #[arg(long, short = 's', value_name = "LABEL")]
        spec: Option<String>,
        /// Override the anchor's `base_commit` for tier-1 detection.
        #[arg(long, value_name = "COMMIT")]
        since: Option<String>,
    },
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
            index,
            id,
            answer,
            dismiss,
        } => run_msg(&workspace, spec, index, id, answer, dismiss),
        Command::Todo { spec, since } => run_todo(&workspace, spec, since, agent_override),
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

fn run_status(workspace: &std::path::Path) -> anyhow::Result<()> {
    let db = loom_core::state::StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
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
                            return SessionResult::PreflightFailed {
                                error: format!("open log sink: {err}"),
                            };
                        }
                    };
                    dispatch_classified(kind, spawn_cfg, shutdown_grace, sink).await
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
    use loom_workflow::run::{AgentOutcome, run_parallel_batch};

    let bd = BdClient::new();
    let beads = bd
        .ready(loom_core::bd::ReadyOpts {
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
                Ok(_) => AgentOutcome::Success,
                Err(e) => AgentOutcome::Failure {
                    error: format!("{e}"),
                },
            }
        }
    })
    .await?;

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
/// [`ProfileError::UnknownProfile`]: loom_core::profile_manifest::ProfileError::UnknownProfile
async fn dispatch_for_slot(
    kind: AgentKind,
    shutdown_grace: Option<Duration>,
    slot: loom_workflow::run::WorktreeBead,
    manifest: &ProfileImageManifest,
    cli_profile: Option<&ProfileName>,
    phase_default: &ProfileName,
    logs_root: &Path,
    label: &SpecLabel,
) -> anyhow::Result<SessionOutcome> {
    use loom_core::agent::RePinContent;
    use loom_core::scratch::ScratchSession;
    use loom_workflow::run::build_spawn_config_from_manifest;

    let initial_prompt = format!("loom run: bead {}", slot.bead.id);
    let banner = format!("loom run @ {}", slot.bead.id);
    // `<key>` is the bead id — parallel run workers on different beads
    // therefore get independent scratch dirs even when sharing a workspace.
    let scratch = ScratchSession::open(
        &slot.worktree.path,
        slot.bead.id.as_str(),
        &initial_prompt,
        &banner,
    )?;
    let spawn_config = build_spawn_config_from_manifest(
        manifest,
        &slot.bead,
        cli_profile,
        phase_default,
        slot.worktree.path.clone(),
        initial_prompt,
        RePinContent {
            orientation: banner,
            pinned_context: String::new(),
            partial_bodies: vec![],
        },
        scratch.path().to_path_buf(),
        vec![],
        vec![],
    )?;

    let sink = open_bead_sink(logs_root, label, &slot.bead.id)?;
    let result = dispatch(kind, spawn_config, shutdown_grace, Some(sink), None).await;
    drop(scratch);
    Ok(result?)
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
        AgentKind::Pi => run_agent_classified::<PiBackend>(&spawn, sink, None).await,
        AgentKind::Claude => run_agent_classified::<ClaudeBackend>(&spawn, sink, None).await,
    }
}

/// Test seam: read a millisecond budget from `name` if set. Production
/// runs leave the env vars unset and SpawnConfig falls back to the
/// constants in `loom_core::agent` (30s handshake / 60s stall warn).
fn duration_env_ms(name: &str) -> Option<Duration> {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis)
}

/// Resolve the configured shutdown grace from the active agent selection.
/// Pi sessions return `None` because pi exits naturally on `agent_end`;
/// claude sessions return the parsed `[claude] post_result_grace_secs`.
fn resolve_shutdown_grace(selection: &loom_core::config::AgentSelection) -> Option<Duration> {
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
) -> anyhow::Result<loom_core::config::AgentSelection> {
    let mut selection = config.agent_for(phase)?;
    if let Some(kind) = agent_override {
        selection.kind = kind;
        selection.claude_settings = match kind {
            AgentKind::Claude => Some(loom_core::config::ClaudeSettings {
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
    index: Option<u32>,
    id: Option<String>,
    answer: Option<String>,
    dismiss: bool,
) -> anyhow::Result<()> {
    let _manifest = ProfileImageManifest::from_env()?;
    let spec_filter = spec.as_deref().map(SpecLabel::new);
    if let Some(label) = &spec_filter {
        let lock_mgr = LockManager::new(workspace)?;
        let _guard = lock_mgr.acquire_spec(label)?;
        run_msg_inner(answer, dismiss, index, id, spec_filter)
    } else {
        run_msg_inner(answer, dismiss, index, id, None)
    }
}

fn run_msg_inner(
    answer: Option<String>,
    dismiss: bool,
    index: Option<u32>,
    id: Option<String>,
    spec_filter: Option<SpecLabel>,
) -> anyhow::Result<()> {
    if answer.is_some() && dismiss {
        anyhow::bail!("use either -a <choice> or -d, not both");
    }

    let runtime = tokio::runtime::Runtime::new()?;
    let beads = runtime.block_on(async {
        let bd = BdClient::new();
        bd.list(ListOpts {
            status: None,
            label: Some("loom:clarify".to_string()),
        })
        .await
    })?;
    let kept = filter_clarifies(&beads, spec_filter.as_ref());

    if answer.is_none() && !dismiss {
        let rows = build_rows(&kept, spec_filter.as_ref());
        if rows.is_empty() {
            println!("(no outstanding clarifies)");
            return Ok(());
        }
        for row in rows {
            match row.spec {
                Some(s) => println!(
                    "{:>3}. {} [spec:{}] {}",
                    row.index, row.bead_id, s, row.summary
                ),
                None => println!("{:>3}. {} {}", row.index, row.bead_id, row.summary),
            }
        }
        return Ok(());
    }

    let (target, _pos) = resolve_target(&kept, index, id.as_deref())?;
    let bead = kept
        .iter()
        .find(|b| b.id == target)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("bead {target} not in filtered list"))?;

    if let Some(choice) = answer {
        let reply = build_fast_reply(&target, &choice, &bead.description)?;
        let note = match &reply {
            FastReply::Option { note, .. } => note.clone(),
            FastReply::Verbatim { note } => note.clone(),
        };
        let runtime = tokio::runtime::Runtime::new()?;
        let id_clone = target.clone();
        runtime.block_on(async move {
            let bd = BdClient::new();
            bd.update(
                &id_clone,
                UpdateOpts {
                    remove_labels: vec!["loom:clarify".to_string()],
                    ..UpdateOpts::default()
                },
            )
            .await
        })?;
        println!("answered {target}: {note}");
        if let Some(label) = spec_label_of(bead) {
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
                    remove_labels: vec!["loom:clarify".to_string()],
                    ..UpdateOpts::default()
                },
            )
            .await
        })?;
        println!("dismissed {target}: {DISMISS_NOTE}");
        if let Some(label) = spec_label_of(bead) {
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
    let db = loom_core::state::StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
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
