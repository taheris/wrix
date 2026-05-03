//! `loom` CLI binary entry point.
//!
//! Parses command-line arguments and dispatches to the workflow modules in
//! `loom-workflow`. The set of subcommands matches the harness specification:
//! `init`, `status`, `use`, `logs`, `spec`, plus the previously-implemented
//! `run`, `check`, `msg`. There is no `sync` or `tune` — Askama compiled
//! templates make per-project sync unnecessary (see `specs/loom-harness.md`).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use loom_core::bd::BdClient;
use loom_core::identifier::{BeadId, SpecLabel};
use loom_workflow::{init, logs_cmd, spec, status, use_spec};

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
