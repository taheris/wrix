//! `loom run` — per-bead execution loop.
//!
//! Implements the sequential (`--parallel 1`) shape of the run command per
//! `specs/loom-harness.md` "Command set" / "Process Architecture" / "Run UX
//! & Logging". The loop:
//!
//! 1. resolves the per-bead profile from the bead's `profile:X` label (or a
//!    `--profile` override) and builds a typed [`SpawnConfig`](
//!    loom_core::agent::SpawnConfig);
//! 2. renders the [`RunContext`](loom_templates::run::RunContext) prompt with
//!    the bead's id/title/description, threading the previous-failure body
//!    (truncated to 4000 chars) on retries;
//! 3. spawns `wrapix run-bead --spawn-config <file> --stdio` via an
//!    [`AgentBackend`](loom_core::agent::AgentBackend) and tees the
//!    [`AgentEvent`](loom_core::agent::AgentEvent) stream into the terminal
//!    renderer + per-bead NDJSON log;
//! 4. on agent failure retries with `previous_failure` injected up to
//!    `max_retries` (default 2), then applies the `loom:clarify` label;
//! 5. on bead success closes the bead;
//! 6. on molecule completion (no more ready beads) execs `loom check` —
//!    continuous mode only.
//!
//! `--parallel N > 1` (worktree parallelism) lives in [`parallel`]. The
//! sequential and parallel paths share the [`AgentOutcome`] / retry vocabulary
//! but split on dispatch: sequential spawns one container on the driver
//! branch; parallel spawns N containers in disjoint worktrees and merges
//! finished branches sequentially.

mod context;
mod error;
mod outcome;
mod parallel;
mod parallelism;
mod production;
mod profile;
mod retry;
mod runner;
mod spawn;

pub use context::{RunContextInputs, build_run_context};
pub use error::RunError;
pub use outcome::{AgentOutcome, BeadResult};
pub use parallel::{
    BatchOutcome, BatchResult, BatchSlot, WorktreeBead, create_worktrees, merge_back,
    run_concurrent_spawns,
};
pub use parallelism::{Parallelism, ParallelismError};
pub use production::{ProductionAgentLoopController, STUB_AGENT_ERROR, list_open_for_spec};
pub use profile::{DEFAULT_PROFILE, resolve_profile};
pub use retry::{RetryDecision, RetryPolicy};
pub use runner::{AgentLoopController, RunMode, RunSummary, run_loop};
pub use spawn::build_spawn_config;
