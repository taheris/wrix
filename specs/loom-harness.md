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

1. **Command set** — Ralph workflow commands ported to Rust, plus a small set
   of auxiliary state/log management commands:

   **Workflow commands (driver phases):**
   - `loom plan` — spec interview (interactive agent session); flags
     `-n <label>` for a new spec and `-u <label>` for updating an existing
     one. No hidden-spec flag: scratch / private specs are kept out of
     git via `.git/info/exclude` rather than a separate spec home.
   - `loom todo` — spec-to-beads decomposition
   - `loom run` — execute beads in loop (continuous or `--once`)
   - `loom check` — review gate + push control
   - `loom msg` — clarify resolution
   - `loom spec` — query spec annotations; supports `--deps` to print
     nixpkgs required by the spec's `[verify]` / `[judge]` test files
     (port of `ralph sync --deps`)

   **Auxiliary commands (state / log management):**
   - `loom init` — create `.wrapix/loom/` config + state DB; `--rebuild`
     repopulates the state DB from `specs/*.md` and active beads
   - `loom status` — print active spec, current molecule, iteration count
     (trivial state DB query)
   - `loom use <label>` — set `current_spec` in the state DB; `loom status`
     reads it back
   - `loom logs` — tail the most recent bead NDJSON log under
     `.wrapix/loom/logs/`; `--bead <id>` selects a specific bead

   **Ralph commands deliberately NOT ported:**
   - `ralph sync` / `ralph tune` — these manage per-project copies of bash
     mustache templates so users can customize them. Loom's templates are
     Askama, compiled into the binary; there is no per-project template
     copy to sync, back up, diff, or tune. Template updates ship via a new
     loom release.
   - `ralph watch` — polling observation daemon (tmux/browser monitoring
     that creates beads for detected issues). Independent feature, not
     part of the workflow phase set; deferred to a follow-up spec.
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
6. **Worktree parallelism** — `loom run --parallel N` (alias `-p N`) dispatches
   up to N ready beads concurrently, each in its own git worktree on a
   per-bead branch. After workers finish, branches are merged back to the
   driver branch sequentially. Default parallelism is 1 (sequential, current
   ralph behavior). Same model as `lib/ralph/cmd/run.sh` (`run_parallel_batch`).
7. **Retry with context** — on worker failure, retries with previous error
   output injected into the prompt. Configurable max retries per bead
   (default 2). After max retries, applies `ralph:clarify` label.
8. **Auto-check handoff** — in continuous `run` mode, invokes `check` when the
   molecule completes (same exec semantics as current bash).
9. **Push gate** — `check` only pushes on clean completion (no new beads, no
   clarifies). Auto-iterates if fix-up beads created (up to max iterations).
10. **Beads via shared Dolt socket** — every container has the host's
    `wrapix-beads` Dolt server bind-mounted at
    `/workspace/.wrapix/dolt.sock`; in-container `bd` writes go straight to
    the authoritative state. No per-bead `bd dolt push/pull` handoff. Loom
    on the host reads the same state through the same socket. The legacy
    `.beads/issues.jsonl` path is not used — beads no longer supports it.
11. **Spec resolution** — `--spec <name>` flag or fallback to the
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

### Process Architecture

Loom is a host-side orchestrator. Every workflow phase that drives an agent
spawns its own container per bead — no shared long-lived container, no
in-container loom loop. The two motivations:

1. **Per-bead profile selection.** Beads carry `profile:rust` /
   `profile:python` / `profile:base` labels. Each bead must run in a container
   built from the matching wrapix profile. A long-lived parent container can't
   change profile mid-run; per-bead spawn is the only clean way.
2. **Trust boundary.** Loom (orchestrator, on host) is trusted; the agent
   (claude or pi, in container) is the sandboxed execution layer.

**Container spawn is delegated to `wrapix run-bead`** — a thin wrapix
subcommand that owns container construction (mounts, env passthrough, krun
runtime selection on aarch64 microVM, network filtering, deploy key, beads
dolt socket). Loom never invokes `podman run` directly. Nix remains the source
of truth for container layout; loom owns only the typed `SpawnConfig` it
hands to the wrapper.

```
loom (host)
    │
    ├─ build SpawnConfig (image, env allowlist, mounts, repin)
    ├─ serialize to /tmp/loom-<id>.json
    │
    ├─ spawn: wrapix run-bead --spawn-config /tmp/loom-<id>.json --stdio
    │   │
    │   └─ exec podman run [no -t, stdio piped] <image> <entrypoint>
    │       │
    │       └─ entrypoint.sh → agent (claude --print … / pi --mode rpc)
    │           ↑              ↓
    │           └── NDJSON over stdin/stdout ─→ loom (parses events)
    │
    └─ on bead completion: container exits, next bead → next spawn
```

`wrapix run-bead --stdio` is the non-TTY counterpart of today's interactive
`wrapix run` (which uses `podman run -it`). Both modes share container
construction; they differ only in stdio attachment. The
`--spawn-config <file>` flag accepts a JSON file that mirrors loom's typed
`SpawnConfig` — avoiding a fat argv interface and giving loom a single
serialization boundary.

**`loom plan` is the exception.** It is an interactive spec interview
(human-in-the-loop terminal session), so it shells out to interactive
`wrapix run` rather than driving an NDJSON session. Loom prepares the
template-rendered prompt, sets environment, exec's `wrapix run`, and lets
claude attach to the user's terminal. No subprocess capture, no NDJSON.

**Trade-off accepted:** parallelism is straightforward (N concurrent
`wrapix run-bead` invocations) but per-bead container spawn cost (~1-2s on
podman) replaces ralph's per-iteration claude spawn inside one long-lived
container. For typical bead sizes (minutes of agent work), this is dominated
by agent runtime.

### Worktree Parallelism

`loom run --parallel N` is v1-parity with ralph's `run_parallel_batch` (see
`lib/ralph/cmd/run.sh`):

1. Pull up to N ready beads (`bd ready --limit=N`).
2. For each bead, create a git worktree at
   `.wrapix/worktree/<label>/<bead-id>/` on a fresh branch
   `loom/<label>/<bead-id>` based on HEAD.
3. Spawn one `wrapix run-bead --spawn-config <file> --stdio` per worktree
   concurrently. Each container's workdir bind mount points at the worktree
   path, not the main checkout.
4. `tokio::join!` (or `JoinSet`) on the futures; collect per-bead results.
5. Merge finished bead branches back to the driver branch **sequentially**
   (single-threaded merge avoids index lock contention). On merge conflict,
   the bead is marked failed and the worktree is preserved for inspection.
6. On agent failure, the worktree branch is cleaned up (deleted) and the
   bead is retried per the retry policy.

`--parallel 1` is the default and behaves exactly as today's sequential run
(no worktree, work happens on the driver branch). `--parallel N` for `N > 1`
always uses worktrees, even for a single ready bead in that batch.

**Git operations: hybrid `gix` + `git` CLI.** Worktree, branch, status,
and merge operations go through a typed `GitClient` in `loom-core`. The
implementation is hybrid, encapsulated inside the module; callers see only
typed Rust methods.

| Operation | Backend | Reason |
|-----------|---------|--------|
| `status` (working tree vs HEAD) | [`gix`](https://docs.rs/gix) `Repository::status()` | mature (`crate-status.md`: checked) |
| `diff` (HEAD vs HEAD~) | `gix::diff` (`blob-diff` feature) | mature |
| List refs / branches | `gix::Repository::references()` | mature |
| Read commit graph / HEAD | `gix` | mature |
| List worktrees | `gix::Repository::worktrees()` | mature (open/iter only) |
| **Create worktree + branch** | `git worktree add -b` (CLI) | `gix` worktree create/remove unchecked in `crate-status.md` |
| **Remove / prune worktree** | `git worktree remove` / `prune` (CLI) | same |
| **Merge bead branch back** | `git merge` (CLI) | `gix-merge` writes a merged tree but cannot persist `MERGE_HEAD`/`MERGE_MSG` (unchecked); avoids reimplementing the index dance |

`gix` 0.83+ is pinned with features `["status", "blob-diff", "revision",
"parallel", "sha1"]` (the `sha1` feature is required for gix-hash to compile;
without it the `Kind` enum has no variants). For tokio integration:
`gix::Repository` is `!Sync`; loom holds a
`ThreadSafeRepository` and clones a thread-local handle inside
`spawn_blocking` per call. CLI shell-outs use `tokio::process::Command`
with arguments passed via `.arg()` — never shell interpolation — and a
60-second timeout matching `BdClient`.

The hybrid line is reviewed each loom release; gix operations migrate
inward as the corresponding `crate-status.md` items become checked.

### Concurrency & Locking

Multiple `loom` invocations on the same workspace are explicitly allowed.
The lock model is **per-spec advisory locks** plus a single workspace
exclusive lock used only during destructive state rebuild.

**Lock files** live under `.wrapix/loom/locks/`:

- `.wrapix/loom/locks/<label>.lock` — one per spec
- `.wrapix/loom/locks/workspace.lock` — held by `loom init` and
  `loom init --rebuild` (`workspace` is reserved as a spec label to avoid
  collision)

All locks are POSIX advisory locks acquired via `flock(2)` through the
`fd-lock` crate. The kernel releases them on process exit or crash, so
there are no stale locks to clean up.

**Lock matrix:**

| Class | Commands | Lock acquired |
|-------|----------|---------------|
| Read-only | `status`, `logs`, `spec` | none |
| Spec-scoped mutating | `plan`, `todo`, `run`, `check`, `msg`, `use` | exclusive on `<label>.lock` |
| Workspace-exclusive | `init`, `init --rebuild` | exclusive on `workspace.lock` |

A spec-scoped command on label `X` waits up to 5 seconds for `<X>.lock`,
then errors with `another loom command is operating on <X>` (no busy-loop,
no silent stalls). `init` and `init --rebuild` error immediately if any
spec lock is held.

**Why git is the second-order serialization point.** Two `loom run`
invocations on *different* specs share the driver git branch. They will
collide briefly at merge-back and push:

- Concurrent `git merge` is serialized by git's own `index.lock`; the
  losing process surfaces a clear error and retries.
- Concurrent `git push` from `loom check` produces non-fast-forward on
  the second push; `check`'s push gate re-fetches and retries.

These are accepted, recoverable failure modes — not silent corruption —
which is why a workspace-wide lock is *not* required for `run`/`check`.

### Run UX & Logging

`loom run` is the only long-running command users watch live. Its terminal
output is shaped for a human reading along, not for machine parsing.

**Default terminal output** (one bead at a time, sequential or parallel):

```
▸ wx-abc123  Implement parser    [profile:rust]
  Read    src/lib.rs
  Read    src/parser/mod.rs
  Edit    src/parser/mod.rs +42 -3
  Bash    cargo test --lib
  ✓ done  (3 tool calls, 47s)
```

Rules:

- One header line per bead: id, title, profile.
- One line per tool call: tool name + a short summary (path, range, command
  prefix). Long values are truncated to one line.
- Assistant text deltas are **not** streamed live by default. The final
  assistant message is summarized in the closing `✓ done` / `✗ failed` line.
- `--verbose` (or `-v`) escalates to live streaming: assistant text deltas
  print as they arrive, plus tool call args inline.
- Color is used for status only (green ✓, red ✗, yellow for retry); tool
  output stays plain so logs are grep-friendly.
- With `--parallel N`, header/finish lines are printed atomically; tool
  call lines are tagged with the bead id prefix so interleaved output
  remains attributable.

**Log persistence.** Loom always writes the **full raw NDJSON** event stream
for every bead to disk, regardless of terminal verbosity. One file per bead
spawn:

```
.wrapix/loom/logs/<spec-label>/<bead-id>-<utc-timestamp>.ndjson
```

Per-bead (not per-session) so parallel batches never interleave inside a
single file. The path is logged at `info!` when the spawn starts so users
can `tail -f` it.

**Retention.** Logs are swept on `loom run` startup: any file under
`.wrapix/loom/logs/` whose mtime is older than `[logs] retention_days`
(default 14) is deleted. `retention_days = 0` disables sweeping (keep
forever). The sweep is best-effort and logged at `debug!` — failures to
delete (permission, in-use file) do not abort the run. Sweeping runs once
per `loom run` invocation, before any bead spawns; the cost is a single
directory walk.

The terminal renderer consumes the same `AgentEvent` stream that's written
to disk — there's a single tee-style sink, not two parallel pipelines.

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
gix = { version = "0.83", default-features = false, features = ["status", "blob-diff", "revision", "parallel", "sha1"] }
fd-lock = "4"
```

Fourteen dependencies. No NDJSON-specific crate — `serde_json` + `BufReader` line
splitting is sufficient. No `async-trait` — `async fn` in traits is stable and works
natively with static dispatch. `gix` covers read-only git operations (status,
diff, refs, commit graph, worktree iteration); worktree mutation and merge
shell out to the system `git` CLI — see *Worktree Parallelism* for the split.

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

**Newtype IDs:**

Each identifier in `loom-core::identifier` is hand-written (no shared macro)
so per-type parse rules can be enforced at construction. The shape:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct $Name(String);

impl $Name {
    pub fn as_str(&self) -> &str { &self.0 }
}

impl std::fmt::Display for $Name { /* writes self.0 */ }
```

`BeadId` additionally validates the canonical
`<prefix>-<base32>(.<digits>)?` shape at every construction path: `new`
returns `Result<Self, ParseBeadIdError>`, and `Deserialize` is hand-written
to reject malformed input rather than constructing an invalid wrapper.
Other newtypes (`SessionId`, `ToolCallId`, `RequestId`, `SpecLabel`,
`MoleculeId`, `ProfileName`) keep a permissive `new(impl Into<String>)`.

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
  unbounded hangs from a stuck `bd` process.
- Key operations: `show`, `create`, `close`, `update`, `list`, `dep_add`,
  `mol_bond`, `mol_progress`. No `dolt_push` / `dolt_pull` wrappers — loom
  relies on the bind-mounted Dolt socket so every `bd` call is already
  authoritative.

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
repopulates from three sources:

1. Glob `specs/*.md` → one `specs` row per file (label from filename, path
   from disk). ~10-20 files.
2. `bd list --status=open --label=ralph:active` → active molecules only
   (typically 0-3). For each, `bd mol progress <id>` → one `molecules` row.
3. Each spec markdown is parsed for a canonical `## Companions` section
   (see *Companion declaration in specs* below); each listed path becomes
   one `companions` row. Specs without the section contribute zero
   companions, not an error.

Iteration counters reset to 0 on rebuild. **Implementation notes are lost
on rebuild** — they're written by `loom plan` and have no external source
to reconstruct from. This is the only field with this property; recovering
notes after a rebuild requires re-running the relevant `loom plan` session.
Total cost: a glob + ~5 `bd` CLI calls + N markdown reads (already loaded
for source #1). Runs in under a second.

**Companion declaration in specs.** Specs declare their companion paths in
a single, parseable section so rebuild is lossless:

```markdown
## Companions

- `lib/sandbox/`
- `lib/ralph/template/`
```

Parser rules:

- Heading must be exactly `## Companions` (case-sensitive, level 2).
- Body is a flat bullet list of `- ` lines. Each path is a single
  backtick-delimited token; anything outside the backticks is ignored.
- Paths are normalized to repo-relative POSIX form (no leading `/`,
  trailing slash preserved if present).
- Missing section → zero companions for this spec (not an error).
- Malformed lines (no backticks, multiple paths) are skipped with a
  `warn!`, not an abort.

This is the only contract between spec authors and the state DB on
companions. The `loom plan` interview enforces the format when adding
companion paths to a spec.

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
2. State is independent: Ralph uses `.wrapix/ralph/state/<label>.json`,
   Loom uses `.wrapix/loom/state.db`. No cross-system state sharing.
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
| `lib/sandbox/linux/default.nix` | Add `wrapix run-bead` subcommand: stdio (no `-it`), accepts `--spawn-config <file>`, otherwise reuses the existing container construction (mounts, env, krun, network filter, deploy key, beads socket) |
| `lib/sandbox/darwin/default.nix` | Same `--spawn-config` plumbing for the Darwin path |

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

### Process architecture

- [ ] Loom never invokes `podman run` directly (grep `loom/crates/` for
      `podman` finds only documentation references)
  [verify](tests/loom-test.sh::test_loom_does_not_invoke_podman)
- [ ] `wrapix run-bead --spawn-config <file> --stdio` accepts a JSON config,
      reuses container construction from existing `wrapix run`, omits TTY
  [verify](tests/loom-test.sh::test_wrapix_run_bead_subcommand)
- [ ] `SpawnConfig` JSON shape is stable: serialization round-trip preserves
      all fields and key names
  [verify](tests/loom-test.sh::test_spawn_config_json_stability)
- [ ] Per-bead profile selection: two beads with different profile labels
      result in two `wrapix run-bead` invocations with different `image`
  [verify](tests/loom-test.sh::test_per_bead_profile_spawn)
- [ ] `loom plan` shells out to interactive `wrapix run` (TTY attached); does
      not capture stdio for NDJSON
  [verify](tests/loom-test.sh::test_plan_uses_interactive_wrapix_run)

### Concurrency & locking

- [ ] Spec-scoped mutating commands acquire `<label>.lock` and release on
      process exit
  [verify](tests/loom-test.sh::test_per_spec_lock_acquired)
- [ ] Two mutating commands on the same spec serialize: the second waits up
      to 5s, then errors clearly
  [verify](tests/loom-test.sh::test_per_spec_lock_serializes)
- [ ] Two mutating commands on *different* specs run concurrently (no
      blocking)
  [verify](tests/loom-test.sh::test_cross_spec_no_blocking)
- [ ] Read-only commands (`status`, `logs`, `spec`) acquire no lock and run
      during an active `loom run`
  [verify](tests/loom-test.sh::test_readonly_commands_unblocked)
- [ ] `loom init` and `loom init --rebuild` acquire the workspace lock
      and error immediately if any per-spec lock is held
  [verify](tests/loom-test.sh::test_init_workspace_lock)
- [ ] Crashed loom process leaves no stale lock (kernel releases flock on
      exit; new invocation acquires immediately)
  [verify](tests/loom-test.sh::test_crash_releases_lock)

### Run UX & logging

- [ ] Default terminal output prints one header line per bead and one short
      line per tool call (no streamed assistant text)
  [verify](tests/loom-test.sh::test_run_default_output_shape)
- [ ] `--verbose` / `-v` enables live assistant text streaming
  [verify](tests/loom-test.sh::test_run_verbose_streams_text)
- [ ] Full raw NDJSON event stream is written to
      `.wrapix/loom/logs/<spec-label>/<bead-id>-<timestamp>.ndjson` for every
      bead spawn, regardless of terminal verbosity
  [verify](tests/loom-test.sh::test_run_writes_per_bead_ndjson_log)
- [ ] Log path is logged at `info!` when the spawn starts
  [verify](tests/loom-test.sh::test_run_logs_log_path)
- [ ] With `--parallel N > 1`, each bead writes to its own file (no
      interleaving in a single log)
  [verify](tests/loom-test.sh::test_parallel_logs_are_per_bead)
- [ ] Terminal renderer and log writer consume the same `AgentEvent` stream
      (single sink, not two pipelines)
  [judge](tests/judges/loom.sh::test_run_single_event_sink)
- [ ] On `loom run` startup, log files older than `[logs] retention_days`
      (default 14) are deleted; recent logs are preserved
  [verify](tests/loom-test.sh::test_log_retention_sweep)
- [ ] `[logs] retention_days = 0` disables sweeping (no files deleted)
  [verify](tests/loom-test.sh::test_log_retention_disabled)
- [ ] Sweep failures (permission denied, in-use file) do not abort the run
  [verify](tests/loom-test.sh::test_log_retention_failure_tolerance)

### Worktree parallelism

- [ ] `loom run --parallel 1` (default) does not create a worktree and works
      on the driver branch directly
  [verify](tests/loom-test.sh::test_parallel_one_no_worktree)
- [ ] `loom run --parallel N` (N > 1) creates one worktree per dispatched bead
      under `.wrapix/worktree/<label>/<bead-id>/`
  [verify](tests/loom-test.sh::test_parallel_creates_worktrees)
- [ ] Each worktree spawns its own `wrapix run-bead` and the spawns run
      concurrently (overlapping wall-clock)
  [verify](tests/loom-test.sh::test_parallel_concurrent_spawns)
- [ ] Successful bead branches are merged back to the driver branch after
      the batch completes
  [verify](tests/loom-test.sh::test_parallel_merge_back)
- [ ] On worker failure, the bead worktree branch is cleaned up and the bead
      is queued for retry per the retry policy
  [verify](tests/loom-test.sh::test_parallel_failure_cleanup)
- [ ] On merge conflict, the worktree is preserved and the bead is marked
      failed (not silently overwritten)
  [verify](tests/loom-test.sh::test_parallel_conflict_preserves_worktree)
- [ ] `GitClient` is the only module that imports `gix` or invokes the `git`
      CLI; callers see typed Rust methods
  [judge](tests/judges/loom.sh::test_git_client_encapsulation)

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
- [ ] `loom run --parallel N` (alias `-p N`) accepts a positive integer; non-
      positive or non-integer values fail with a clear error
  [verify](tests/loom-test.sh::test_run_parallel_flag_validation)
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
- [ ] `loom spec --deps` scans verify/judge test files in the active spec
      and prints required nixpkgs (port of `ralph sync --deps`)
  [verify](tests/loom-test.sh::test_spec_deps)

### Auxiliary commands

- [ ] `loom init` creates `.wrapix/loom/config.toml` and `.wrapix/loom/state.db`
      with the default schema
  [verify](tests/loom-test.sh::test_init_creates_state)
- [ ] `loom init --rebuild` repopulates the state DB from `specs/*.md`
      and active beads
  [verify](tests/loom-test.sh::test_init_rebuild)
- [ ] `loom status` prints active spec, current molecule, iteration count
      from the state DB
  [verify](tests/loom-test.sh::test_status_command)
- [ ] `loom use <label>` sets `current_spec` in the state DB; round-trips
      with `loom status`
  [verify](tests/loom-test.sh::test_use_command)
- [ ] `loom logs` tails the most recent NDJSON log under
      `.wrapix/loom/logs/`; `--bead <id>` selects a specific bead's log
  [verify](tests/loom-test.sh::test_logs_command)
- [ ] No `loom sync` / `loom tune` commands exist (compiled templates make
      them unnecessary)
  [verify](tests/loom-test.sh::test_no_sync_or_tune_command)

### State database

- [ ] `StateDb::open` creates tables on first open
  [verify](tests/loom-test.sh::test_state_db_init)
- [ ] `StateDb::rebuild` populates from spec files and active beads
  [verify](tests/loom-test.sh::test_state_db_rebuild)
- [ ] `StateDb::rebuild` parses each spec's `## Companions` section and
      writes one `companions` row per listed path; specs without the
      section contribute zero rows (not an error)
  [verify](tests/loom-test.sh::test_state_db_rebuild_companions)
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
- **Parallelism beyond v1 parity** — `loom run --parallel N` matches ralph's
  worktree-per-bead model and feature set. New parallelism strategies
  (cross-spec, distributed, scheduler-aware) are future work.
- **Hidden specs (`-h` flag)** — Ralph's hidden-spec mode (spec lives in
  `.wrapix/ralph/state/<label>.md`, not committed, single-spec only) is
  deliberately not ported. It's a degraded mode (no siblings, no
  companions, no fan-out) whose use case — keeping a spec out of git —
  is covered by `.git/info/exclude` on `specs/<label>.md`. Removing the
  flag eliminates an asymmetric branch from `plan`/`todo`/`run`
  path-resolution. Reintroducing it later is a non-breaking additive
  change if the workflow asks for it.
- **Per-project template customization** — Loom templates are Askama,
  compiled into the binary. `ralph sync` (template fetch/backup/diff)
  and `ralph tune` (AI-assisted template editing) have no v1 equivalent.
  Project-specific prompt tweaks happen via `pinned_context` and the
  per-spec implementation-notes mechanism, not by editing template
  source.
- **Observation daemon (`ralph watch`)** — polling monitor that spawns
  short-lived agent sessions to observe tmux/browser logs and create
  beads. Independent of the workflow phase set; deferred to a follow-up
  spec.
- **Session persistence across container restarts** — each container starts a
  fresh agent session.

## Configuration

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

[logs]
# Delete log files under .wrapix/loom/logs/ older than this many days on
# `loom run` startup. 0 disables sweeping (keep forever).
retention_days = 14

[exit_signals]
complete = "LOOM_COMPLETE"
blocked = "LOOM_BLOCKED"
clarify = "LOOM_CLARIFY"

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

[claude]
# Seconds to wait for clean exit after `result` before SIGTERM. Mirrors
# ralph's RALPH_CLAUDE_POST_RESULT_GRACE.
post_result_grace_secs = 5

[security]
# Tool names to deny when Claude sends control_request. Claude-backend only —
# pi has no host-side permission flow (tools execute internally, no
# control_request analog). Empty by default — the container sandbox is the
# trust boundary.
# denied_tools = ["SomeNewHostTool"]
```

Defaults match Ralph's so users can transition without configuring Loom
separately. Settings Ralph has that Loom doesn't need (output display,
hooks, watch, failure patterns) are omitted — Loom handles those concerns
in Rust code, not config.
