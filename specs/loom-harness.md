# Loom Harness

Rust workspace, build system, templates, and dual-path transition for the Loom
agent driver.

## Problem Statement

Ralph's workflow (plan, todo, run, check, msg, spec) is implemented as bash
scripts with string-typed data, implicit contracts, and runtime template
rendering. This creates limited type safety — state management, template
variables, bead lifecycle, and error handling are fragile. Loom replaces these
with a Rust binary that owns the full workflow, with compile-time template
validation and typed domain objects.

This spec covers the platform: crate structure, Rust conventions, Nix
integration, Askama templates, SQLite state store, beads CLI wrapper, and the
dual-path transition strategy. The agent abstraction layer (pi-mono and Claude
Code backends, container communication, backend selection) lives in
[loom-agent.md](loom-agent.md). Workflow semantics (what each command does) are
defined in [ralph-loop.md](ralph-loop.md) and
[ralph-review.md](ralph-review.md) — Loom reimplements those semantics in Rust.

## Requirements

### Functional

1. **Full command set** — all Ralph commands reimplemented in Rust:
   - `loom plan` — spec interview (interactive agent session)
   - `loom todo` — spec-to-beads decomposition
   - `loom run` — execute beads in loop (continuous or `--once`)
   - `loom check` — review gate + push control
   - `loom msg` — clarify resolution
   - `loom spec` — query spec annotations
2. **Compiled templates** — Askama templates compiled into the binary. Ralph's
   markdown templates ported to Askama format with typed context structs.
   Template correctness verified at compile time.
3. **SQLite state store** — workflow state persisted in a SQLite database
   (`.wrapix/loom/state.db`). Tracks active specs, molecules, iteration
   counts, companions. Reconstructable from spec files on disk and active
   beads via `loom init --rebuild`.
4. **Beads integration** — interacts with beads via the `bd` CLI (subprocess
   calls). Bead operations: create, show, close, update, list, dep add, mol
   bond, mol progress. CLI output parsed into typed Rust structs.
5. **Profile selection** — reads `profile:X` labels from beads and spawns
   containers with the corresponding wrapix profile (base, rust, python).
   `--profile` flag overrides bead labels.
6. **Retry with context** — on worker failure, retries with previous error
   output injected into the prompt. Configurable max retries per bead
   (default 2). After max retries, applies `ralph:clarify` label.
7. **Auto-check handoff** — in continuous `run` mode, invokes `check` when the
   molecule completes (same exec semantics as current bash).
8. **Push gate** — `check` only pushes on clean completion (no new beads, no
   clarifies). Auto-iterates if fix-up beads created (up to max iterations).
9. **Container bead sync** — maintains the push-inside/pull-outside protocol:
   `bd dolt push` runs inside the container before exit, `bd dolt pull` runs
   on the host after the container exits.
10. **Spec resolution** — `--spec <name>` flag or fallback to the
    `current_spec` key in the state database.

### Non-Functional

1. **Rust edition 2024** with `resolver = "3"` at workspace root.
2. **Workspace-pinned dependencies** — every third-party crate pinned once
   under `[workspace.dependencies]`. Member crates use
   `foo = { workspace = true }`.
3. **Workspace-declared lints** — `[workspace.lints.rust]` and
   `[workspace.lints.clippy]` own the lint block. Every member declares
   `[lints] workspace = true`.
4. **Per-module error enums** using `thiserror` for `Error` and `displaydoc`
   for `Display`. Messages in doc comments, not `#[error("...")]`.
5. **Nested directory module structure** — no central `types.rs` or `error.rs`.
   Types and errors live in the module that owns them. `lib.rs` has `pub mod`
   only.
6. **Parse, Don't Validate** — raw strings parsed into typed representations at
   boundaries. Downstream code never touches raw input.
7. **Newtypes for identifiers** — `BeadId`, `SpecLabel`, `MoleculeId`,
   `ProfileName` for domain identifiers; `SessionId`, `ToolCallId`,
   `RequestId` for protocol identifiers. No bare `String` for typed IDs.
   `AgentKind` is an enum (`Pi`, `Claude`), not a newtype.
8. **No `derive(From)` or `derive(Into)` on newtypes** — bypasses validation.
   `#[from]` only on error enum variants.
9. **No panics in production** — `unwrap()`, `todo!()`, `unimplemented!()`,
   `panic!()` banned. Return error variants. `#[expect(dead_code)]` not
   `#[allow(dead_code)]`.
10. **Structured logging via `tracing`** — log level signals continuation:
    `error!` = failed, `warn!` = continued, `info!` = operational,
    `debug!`/`trace!` = diagnostics. Every log carries structured fields.
    Environment variable values and API keys are never logged — use a
    `Redacted(&str)` wrapper that implements `fmt::Debug` as `"[REDACTED]"`
    for any value that could contain secrets. Variable *names* may be logged.
11. **Nix integration** — built via `buildRustPackage` or `crane` in the
    existing flake. Binary included in the devShell alongside (not replacing)
    Ralph's bash scripts during transition.

## Architecture

### Crate Layout

```
loom/
  Cargo.toml                    # workspace root
  crates/
    loom/                       # CLI binary — clap arg parsing, entry point,
      src/                      #   match on AgentKind, wires concrete
        main.rs                 #   backends + workflow + templates
    loom-core/                  # shared types: BeadId, SpecLabel, MoleculeId,
      src/                      #   AgentBackend trait, AgentEvent enum,
        lib.rs                  #   AgentSession, LineParse, NdjsonReader,
                                #   state db, config, bd CLI wrapper
    loom-agent/                 # AgentBackend trait implementations
      src/                      #   (see loom-agent.md)
        lib.rs
    loom-workflow/              # workflow engine: plan, todo, run, check,
      src/                      #   msg, spec — owns the orchestration loop,
        lib.rs                  #   bead lifecycle, retry logic, push gate
    loom-templates/             # askama templates compiled from ralph's
      src/                      #   markdown; typed context structs per
        lib.rs                  #   template
      templates/                # askama template files (.md)
        plan_new.md
        plan_update.md
        todo_new.md
        todo_update.md
        run.md
        check.md
        msg.md
        partial/
          context_pinning.md
          exit_signals.md
          spec_header.md
          companions_context.md
          ...
```

### Dependency Graph

```
loom (CLI binary)
  ├── loom-core
  ├── loom-agent
  ├── loom-workflow
  └── loom-templates

loom-agent
  └── loom-core

loom-workflow
  ├── loom-core (AgentBackend trait, AgentEvent, newtypes)
  └── loom-templates

loom-templates
  └── loom-core (for typed context structs)
```

### Workspace Dependencies

All third-party crates pinned once under `[workspace.dependencies]`. Member
crates use `foo = { workspace = true }`.

```toml
[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = { version = "1", features = ["raw_value"] }
thiserror = "2"
displaydoc = "0.2"
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
rusqlite = { version = "0.32", features = ["bundled"] }
toml = "0.8"
askama = "0.16"
clap = { version = "4", features = ["derive"] }
```

Twelve dependencies. No NDJSON-specific crate — `serde_json` + `BufReader` line
splitting is sufficient. No `async-trait` — `async fn` in traits is stable and works
natively with static dispatch.

### Parse, Don't Validate

Raw data enters typed domain representations at the boundary and stays typed
everywhere downstream. No internal function re-checks or re-parses.

**Boundary layers (outside → inside):**

1. **NDJSON framing** — `BufReader::read_line` splits the byte stream into
   lines. Each line is one JSON object.
2. **Protocol parsing** — `serde_json::from_str` deserializes each line into a
   backend-specific message type (`PiMessage` or `ClaudeMessage`).
3. **Event normalization** — backend-specific messages map to `AgentEvent`.
   After this point, no code knows which backend is running.
4. **Domain newtypes** — identifiers (`BeadId`, `SpecLabel`, etc.) are parsed
   from strings at construction. Downstream code receives `BeadId`, never
   `String`.
5. **State queries** — SQLite rows map to typed Rust structs via `rusqlite`.
   No intermediate untyped step.
6. **CLI output parsing** — `bd --json` output deserializes into typed structs
   (`Bead`, `Molecule`).

**Newtype ID macro:**

```rust
macro_rules! newtype_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

newtype_id!(BeadId);
newtype_id!(SessionId);
newtype_id!(ToolCallId);
newtype_id!(RequestId);
newtype_id!(SpecLabel);
newtype_id!(MoleculeId);
newtype_id!(ProfileName);
```

`#[serde(transparent)]` means the newtype serializes as a plain string — no
wrapper object. `derive(From)` and `derive(Into)` are banned (NF-8) to prevent
accidental bypass of the newtype boundary.

### Askama Template System

Ralph templates use `{{VARIABLE}}` and `{{> partial}}` syntax. Askama uses
`{{ variable }}` and `{% include "partial.md" %}`. Migration is mechanical:

- `{{LABEL}}` becomes `{{ label }}`
- `{{> context-pinning}}` becomes `{% include "partial/context_pinning.md" %}`
- Each template gets a typed struct with all variables as fields
- Partials become standalone Askama templates included via `{% include %}`
- Agent-generated content (`previous_failure`, `title`, `description`,
  `existing_tasks`) is delimited with `<agent-output>` / `</agent-output>`
  markers in templates to help the receiving agent distinguish injected
  content from system instructions. This is a best-effort mitigation —
  prompt injection via retry context is an inherent risk of the
  retry-with-context pattern and is accepted because the agent runs inside
  a sandboxed container regardless.

Template variables (sourced from
[ralph-harness.md](ralph-harness.md#template-variables)):

| Variable | Type | Used By |
|----------|------|---------|
| `pinned_context` | `String` | all |
| `label` | `SpecLabel` | all |
| `spec_path` | `String` | all |
| `spec_diff` | `Option<String>` | todo-update |
| `existing_tasks` | `Option<String>` | todo-update |
| `companion_paths` | `Vec<String>` | plan-update, todo-*, run, check, msg |
| `clarify_beads` | `Vec<ClarifyBead>` | msg |
| `implementation_notes` | `Vec<String>` | todo-new, todo-update |
| `molecule_id` | `Option<MoleculeId>` | todo-update, run |
| `issue_id` | `Option<BeadId>` | run |
| `title` | `Option<String>` | run |
| `description` | `Option<String>` | run |
| `beads_summary` | `Option<String>` | check |
| `base_commit` | `Option<String>` | check |
| `previous_failure` | `Option<String>` | run (retry only, truncated to 4000 chars) |
| `exit_signals` | `String` | all (via partial) |

### Beads CLI Wrapper

`loom-core` provides `BdClient`, a typed wrapper around the `bd` CLI:

- Invokes `bd` via `tokio::process::Command` with each argument passed via
  `.arg()`. No shell interpolation — values from agent output (bead titles,
  error messages, labels) must never be passed through `sh -c` or string
  interpolation into a shell command. This prevents injection of shell
  metacharacters from agent-controlled content.
- Uses `--json` flag where available
- Parses output into typed structs (`Bead`, `Molecule`, `MolProgress`)
- Maps CLI errors to typed error variants
- All subprocess calls have a 60-second timeout (configurable). Prevents
  unbounded hangs from a stuck `bd` process or oversized `dolt pull`.
- Key operations: `show`, `create`, `close`, `update`, `list`, `dep_add`,
  `mol_bond`, `mol_progress`, `dolt_push`, `dolt_pull`

### SQLite State Store

Workflow state lives in `.wrapix/loom/state.db`. The schema is owned by
`loom-core` and migrated on open (embed migrations via `rusqlite`'s
`execute_batch`).

```sql
CREATE TABLE specs (
    label                TEXT PRIMARY KEY,
    spec_path            TEXT NOT NULL,
    implementation_notes TEXT  -- JSON array, nullable
);

CREATE TABLE molecules (
    id              TEXT PRIMARY KEY,
    spec_label      TEXT NOT NULL REFERENCES specs(label),
    base_commit     TEXT,
    iteration_count INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE companions (
    spec_label     TEXT NOT NULL REFERENCES specs(label),
    companion_path TEXT NOT NULL,
    PRIMARY KEY (spec_label, companion_path)
);

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- meta rows: current_spec, schema_version
```

Typed Rust API — no raw SQL outside `loom-core`:

```rust
pub struct StateDb {
    conn: rusqlite::Connection,
}

impl StateDb {
    pub fn open(path: &Path) -> Result<Self, StateError>;
    pub fn spec(&self, label: &SpecLabel) -> Result<SpecRow, StateError>;
    pub fn active_molecule(&self, label: &SpecLabel) -> Result<Option<MoleculeRow>, StateError>;
    pub fn current_spec(&self) -> Result<Option<SpecLabel>, StateError>;
    pub fn set_current_spec(&self, label: &SpecLabel) -> Result<(), StateError>;
    pub fn increment_iteration(&self, mol_id: &MoleculeId) -> Result<u32, StateError>;
    pub fn rebuild(&self, workspace: &Path, bd: &BdClient) -> Result<RebuildReport, StateError>;
}
```

**Rebuild (`loom init --rebuild`):** Drops and recreates all tables, then
repopulates from two sources:

1. Glob `specs/*.md` → one `specs` row per file (label from filename, path
   from disk). ~10-20 files.
2. `bd list --status=open --label=ralph:active` → active molecules only
   (typically 0-3). For each, `bd mol progress <id>` → one `molecules` row.

Iteration counters reset to 0 on rebuild. Total cost: a glob + ~5 `bd` CLI
calls. Runs in under a second.

**Container exposure:** The state DB is inside the workspace bind-mounted
into containers. A malicious agent could modify it directly. This is an
accepted risk — the DB is reconstructable via `loom init --rebuild`, the
blast radius is limited to iteration counters and `current_spec`, and the
durable sources of truth (spec files on disk, beads in Dolt) are
independently verifiable.

### Compaction Re-Pin

Both backends need the same re-pin payload after compaction — orientation
text, pinned context, and partial response bodies that restore the agent's
working memory. `loom-core` owns a shared `RePinContent` struct that the
workflow engine builds once per session:

```rust
pub struct RePinContent {
    pub orientation: String,
    pub pinned_context: String,
    pub partial_bodies: Vec<String>,
}

impl RePinContent {
    pub fn to_prompt(&self) -> String;
    pub fn write_claude_files(&self, runtime_dir: &Path) -> Result<(), io::Error>;
}
```

The content is identical — only the delivery mechanism differs per backend
(see [loom-agent.md — Compaction Handling](loom-agent.md#compaction-handling)):

- **Claude**: `write_claude_files` writes `repin.sh` + `claude-settings.json`
  before spawn. Claude's `SessionStart` hook reads them on each compaction.
- **Pi**: `to_prompt` renders the content as a prompt string. The driver
  sends it via `steer` command when `compaction_start` arrives in the event
  stream.

## Dual-Path Transition

During the transition period:

1. Both `ralph` (bash) and `loom` (Rust) are available in the devShell.
2. State is independent: Ralph uses `state/<label>.json`, Loom uses
   `.wrapix/loom/state.db`. No cross-system state sharing.
3. Both use the same beads database, `bd` CLI, and spec files.
4. Users can switch between them freely. Switching from Ralph to Loom on
   an in-flight spec requires `loom init --rebuild` to populate the DB.
5. `ralph` remains the default; `loom` is opt-in.
6. Once loom reaches feature parity and stability, `ralph` bash scripts are
   removed and `loom` is renamed to (or aliased as) `ralph`.

### Compatibility Constraints

- Bead labels, molecule IDs, and beads sync protocol are unchanged.
- Git commit conventions are unchanged.
- Container profile selection logic is unchanged.
- Entrypoint.sh changes are additive (agent selection conditional).

## Affected Files

### New

| File | Role |
|------|------|
| `loom/Cargo.toml` | Workspace root |
| `loom/crates/loom/` | CLI binary |
| `loom/crates/loom-core/` | Shared types, traits, bd wrapper, config, state db |
| `loom/crates/loom-agent/` | AgentBackend implementations (see [loom-agent.md](loom-agent.md)) |
| `loom/crates/loom-workflow/` | Workflow engine (plan, todo, run, check, msg, spec) |
| `loom/crates/loom-templates/` | Askama templates ported from Ralph |
| `.wrapix/loom/config.toml` | Loom config (TOML, independent of Ralph's Nix config) |
| `.wrapix/loom/state.db` | SQLite state database (created on first run) |

### Modified

| File | Change |
|------|--------|
| `flake.nix` | Add loom as a Rust package input |
| `modules/flake/packages.nix` | Build and expose loom binary |
| `modules/flake/devshell.nix` | Include loom in devShell |

### Unchanged (during dual-path phase)

| File | Reason |
|------|--------|
| `lib/ralph/cmd/*.sh` | Bash scripts remain functional |
| `lib/ralph/template/` | Templates remain for bash path |
| `lib/city/` | Gas City unchanged |

## Success Criteria

### Crate structure

- [ ] Workspace builds with `cargo build` from `loom/` root
  [verify](tests/loom-test.sh::test_workspace_builds)
- [ ] All five crates present: loom, loom-core, loom-agent, loom-workflow, loom-templates
  [verify](tests/loom-test.sh::test_crate_structure)
- [ ] Workspace uses edition 2024 and resolver "3"
  [verify](tests/loom-test.sh::test_workspace_edition)
- [ ] All dependencies pinned under `[workspace.dependencies]`
  [verify](tests/loom-test.sh::test_workspace_deps_pinned)
- [ ] All crates declare `[lints] workspace = true`
  [verify](tests/loom-test.sh::test_workspace_lints)
- [ ] No `types.rs` or `error.rs` files at crate roots
  [judge](tests/judges/loom.sh::test_nested_module_structure)
- [ ] Domain identifiers use newtypes (BeadId, SpecLabel, MoleculeId, etc.)
  [judge](tests/judges/loom.sh::test_newtypes_for_identifiers)
- [ ] No `unwrap()`, `todo!()`, `panic!()`, `unimplemented!()` in non-test code
  [verify](tests/loom-test.sh::test_no_panics_in_production)
- [ ] No `#[allow(dead_code)]` in non-test code
  [verify](tests/loom-test.sh::test_no_allow_dead_code)
- [ ] No `derive(From)` or `derive(Into)` on newtype structs
  [verify](tests/loom-test.sh::test_no_derive_from_on_newtypes)

### Templates

- [ ] All Ralph templates ported to Askama format
  [verify](tests/loom-test.sh::test_askama_templates_compile)
- [ ] Each template has a typed context struct with all required variables
  [judge](tests/judges/loom.sh::test_template_context_structs)
- [ ] Templates compile at build time (missing variables are compile errors)
  [verify](tests/loom-test.sh::test_template_compile_time_check)
- [ ] Rendered output matches Ralph's bash-rendered output for identical inputs
  [verify](tests/loom-test.sh::test_template_output_parity)
- [ ] Partials included via Askama's `{% include %}` mechanism
  [verify](tests/loom-test.sh::test_template_partials)

### Workflow commands

- [ ] `loom plan -n <label>` spawns container with base profile, runs spec interview
  [verify](tests/loom-test.sh::test_plan_new)
- [ ] `loom plan -u <label>` updates existing spec with anchor/sibling support
  [verify](tests/loom-test.sh::test_plan_update)
- [ ] `loom todo` implements four-tier detection with per-spec cursor fan-out
  [verify](tests/loom-test.sh::test_todo_tier_detection)
- [ ] `loom run` continuous mode processes beads until molecule complete
  [verify](tests/loom-test.sh::test_run_continuous)
- [ ] `loom run --once` processes single bead then exits
  [verify](tests/loom-test.sh::test_run_once)
- [ ] `loom run` reads profile from bead label and spawns correct container
  [verify](tests/loom-test.sh::test_run_profile_selection)
- [ ] `loom run` retries failed beads with previous error context
  [verify](tests/loom-test.sh::test_run_retry_with_context)
- [ ] `loom run` execs `loom check` on molecule completion
  [verify](tests/loom-test.sh::test_run_execs_check)
- [ ] `loom check` implements push gate (push only on clean completion)
  [verify](tests/loom-test.sh::test_check_push_gate)
- [ ] `loom check` auto-iterates on fix-up beads (up to max iterations)
  [verify](tests/loom-test.sh::test_check_auto_iterate)
- [ ] `loom msg` lists outstanding clarify beads
  [verify](tests/loom-test.sh::test_msg_list)
- [ ] `loom msg -a` handles fast-reply (option selection and free-form)
  [verify](tests/loom-test.sh::test_msg_fast_reply)
- [ ] `loom spec` queries spec annotations (verify/judge)
  [verify](tests/loom-test.sh::test_spec_query)

### State database

- [ ] `StateDb::open` creates tables on first open
  [verify](tests/loom-test.sh::test_state_db_init)
- [ ] `StateDb::rebuild` populates from spec files and active beads
  [verify](tests/loom-test.sh::test_state_db_rebuild)
- [ ] `StateDb::rebuild` resets iteration counters to 0
  [verify](tests/loom-test.sh::test_state_db_rebuild_resets_counters)
- [ ] `current_spec` / `set_current_spec` round-trips correctly
  [verify](tests/loom-test.sh::test_state_current_spec)
- [ ] `increment_iteration` returns updated count
  [verify](tests/loom-test.sh::test_state_increment_iteration)
- [ ] Corrupted DB file → `loom init --rebuild` recovers
  [verify](tests/loom-test.sh::test_state_corruption_recovery)

### Beads CLI wrapper

- [ ] `bd show` output parsed into typed `Bead` struct
  [verify](tests/loom-test.sh::test_bd_show_parsing)
- [ ] `bd list` output parsed with label and status filtering
  [verify](tests/loom-test.sh::test_bd_list_parsing)
- [ ] `bd create` returns created bead ID
  [verify](tests/loom-test.sh::test_bd_create_returns_id)
- [ ] CLI errors mapped to typed error variants
  [verify](tests/loom-test.sh::test_bd_error_handling)

### Nix integration

- [ ] Loom binary builds via `nix build`
  [verify](tests/loom-test.sh::test_nix_build)
- [ ] Loom available in devShell alongside ralph
  [verify](tests/loom-test.sh::test_devshell_includes_loom)
- [ ] `cargo clippy` passes with workspace lints
  [verify](tests/loom-test.sh::test_clippy_clean)
- [ ] `cargo test` passes for all crates
  [verify](tests/loom-test.sh::test_cargo_test)

## Out of Scope

- **Deleting Ralph's bash scripts** — happens after Loom reaches parity, not in
  this spec.
- **Gas City integration** — Gas City is experimental and token-heavy. Loom
  does not need to integrate with or replace Gas City's agent management.
- **Agent backend implementations** — defined in [loom-agent.md](loom-agent.md).
- **Workflow semantics changes** — Loom reimplements existing behavior from
  ralph-loop.md and ralph-review.md. No new workflow features.
- **Multiple concurrent agents** — Loom's run loop is sequential (one bead at a
  time). Parallelism is a future enhancement.
- **Session persistence across container restarts** — each container starts a
  fresh agent session.

## Implementation Notes

### Config file

Loom reads `.wrapix/loom/config.toml` — its own config file, independent of
Ralph's `.wrapix/ralph/config.nix`. Parsed natively via the `toml` crate into
a typed `LoomConfig` struct with `#[serde(default)]` on all fields so the file
can be empty or absent (defaults apply).

```toml
pinned_context = "docs/README.md"

[beads]
priority = 2
default_type = "task"

[loop]
max_iterations = 3
max_retries = 2
max_reviews = 2

[exit_signals]
complete = "RALPH_COMPLETE"
blocked = "RALPH_BLOCKED:"
clarify = "RALPH_CLARIFY:"

[agent]
default = "claude"

# Per-phase overrides: backend + model. Phases without overrides inherit default.
# [agent.todo]
# backend = "pi"
# provider = "deepseek"
# model_id = "deepseek-v3"
#
# [agent.check]
# backend = "claude"

[security]
# Tool names to deny when Claude sends control_request.
# Empty by default — the container sandbox is the trust boundary.
# denied_tools = ["SomeNewHostTool"]
```

Defaults match Ralph's so users can transition without configuring Loom
separately. Settings Ralph has that Loom doesn't need (output display,
hooks, watch, failure patterns) are omitted — Loom handles those concerns
in Rust code, not config.
