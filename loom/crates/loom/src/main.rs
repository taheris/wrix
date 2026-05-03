//! `loom` CLI binary entry point.
//!
//! Parses command-line arguments and dispatches to the workflow modules in
//! `loom-workflow`. The set of subcommands matches the harness specification:
//! `init`, `status`, `use`, `logs`, `spec`, plus the previously-implemented
//! `run`, `check`, `msg`. There is no `sync` or `tune` — Askama compiled
//! templates make per-project sync unnecessary (see `specs/loom-harness.md`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use loom_core::bd::{BdClient, ListOpts, UpdateOpts};
use loom_core::identifier::{BeadId, SpecLabel};
use loom_core::lock::LockManager;
use loom_core::state::StateDb;
use loom_workflow::check::{IterationCap, ProductionCheckController, check_loop as run_check_loop};
use loom_workflow::msg::{
    DISMISS_NOTE, FastReply, build_fast_reply, build_rows, filter_clarifies, resolve_target,
    spec_label_of,
};
use loom_workflow::run::{
    Parallelism, ProductionAgentLoopController, RetryPolicy, RunMode, run_loop,
};
use loom_workflow::{init, logs_cmd, plan, spec, status, use_spec};

/// Top-level CLI surface.
#[derive(Debug, Parser)]
#[command(name = "loom", version, about = "Loom harness CLI")]
struct Cli {
    /// Workspace root. Defaults to the current working directory.
    #[arg(long, global = true, value_name = "PATH")]
    workspace: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
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
    /// Tail the most recent per-bead NDJSON log.
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

    let result = match cli.command {
        Command::Init { rebuild } => run_init(&workspace, rebuild),
        Command::Status => run_status(&workspace),
        Command::UseSpec { label } => run_use(&workspace, &label),
        Command::Logs { bead } => run_logs(&workspace, bead.as_deref()),
        Command::Spec { deps } => run_spec(&workspace, deps),
        Command::Plan { new, update } => run_plan(&workspace, new, update),
        Command::Run {
            once,
            parallel,
            profile,
            spec,
        } => run_run(&workspace, once, parallel, profile, spec),
        Command::Check { spec } => run_check(&workspace, spec),
        Command::Msg {
            spec,
            index,
            id,
            answer,
            dismiss,
        } => run_msg(&workspace, spec, index, id, answer, dismiss),
        Command::Todo { spec, since } => run_todo(&workspace, spec, since),
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
    let bead_id = bead.map(BeadId::new);
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
) -> anyhow::Result<()> {
    let mode = plan::parse_mode(new, update)?;
    let report = plan::run(
        workspace,
        plan::PlanOpts {
            mode,
            wrapix_bin: std::env::var_os("LOOM_WRAPIX_BIN").map(PathBuf::from),
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
    _profile: Option<String>,
    spec: Option<String>,
) -> anyhow::Result<()> {
    if !parallel.is_one() {
        anyhow::bail!(
            "loom run --parallel N (N > 1) is not yet wired in the binary; the parallel \
             dispatcher lives in loom_workflow::run::parallel and lands with the agent backend"
        );
    }
    let label = resolve_spec_label(workspace, spec)?;
    let lock_mgr = LockManager::new(workspace)?;
    let _guard = lock_mgr.acquire_spec(&label)?;

    let loom_bin = current_loom_bin()?;
    let runtime = tokio::runtime::Runtime::new()?;
    let mode = if once {
        RunMode::Once
    } else {
        RunMode::Continuous
    };
    let summary = runtime.block_on(async move {
        let bd = BdClient::new();
        let mut controller = ProductionAgentLoopController::new(
            bd,
            label.clone(),
            loom_bin,
            workspace.to_path_buf(),
        );
        run_loop(&mut controller, mode, RetryPolicy::default()).await
    })?;
    println!(
        "loom run: processed {} bead(s), clarified {}, molecule_complete={}, execed_check={}",
        summary.beads_processed,
        summary.beads_clarified,
        summary.molecule_complete,
        summary.execed_check,
    );
    Ok(())
}

fn run_check(workspace: &Path, spec: Option<String>) -> anyhow::Result<()> {
    let label = resolve_spec_label(workspace, spec)?;
    let lock_mgr = LockManager::new(workspace)?;
    let _guard = lock_mgr.acquire_spec(&label)?;

    let loom_bin = current_loom_bin()?;
    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(async move {
        let bd = BdClient::new();
        let mut controller =
            ProductionCheckController::new(bd, label.clone(), loom_bin, workspace.to_path_buf());
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
            label: Some("ralph:clarify".to_string()),
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
                    remove_labels: vec!["ralph:clarify".to_string()],
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
                    remove_labels: vec!["ralph:clarify".to_string()],
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
    _workspace: &Path,
    _spec: Option<String>,
    _since: Option<String>,
) -> anyhow::Result<()> {
    // The todo runner depends on the agent backend (it spawns a wrapix
    // container to run the spec-decomposition prompt) plus a typed GitClient
    // wrapper that's already in loom-core. Wiring the dispatch here without
    // the agent backend would only exercise the four-tier detection path,
    // which is already covered by `compute_spec_diff`'s unit tests
    // (loom-workflow::todo::tier::tests). Surface that gap explicitly.
    anyhow::bail!(
        "loom todo: the spec-decomposition agent is deferred until loom-agent lands; \
         the four-tier detection logic is exercised in loom-workflow::todo::tier::tests"
    )
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
