# Loom Tests

Test strategy and infrastructure for the Loom agent driver.

## Problem Statement

Loom is a Rust binary replacing Ralph's bash workflow scripts with a
multi-crate workspace. Testing has to cover three things at once: protocol
parsing across two agent backends (pi-mono RPC, Claude stream-json), workflow
orchestration (state DB, locking, worktree parallelism, push gate), and
host↔container plumbing (entrypoint branching, bind mounts, profile
selection). The Ralph test suite ([ralph-tests.md](ralph-tests.md))
validates bash behavior with a mock Claude; Loom needs equivalent
coverage in a Rust-native framework with first-class state-DB tests.

This spec designs the test strategy across three tiers — unit, integration,
container smoke — and the design rules that make tests deterministic and
findable: an annotation contract that ties acceptance criteria to executable
tests, a `Clock` trait that eliminates real-time waits, AST-based style
enforcement, snapshot testing for contract surfaces, property-based testing
for protocol parsers, and Nix-pinned protocol versions to catch upstream
drift.

## Requirements

### Functional

1. **Three test tiers** with complementary scope:
   - **Unit tests** (`cargo nextest run`) — per-crate, fast, no external
     dependencies. Inline `#[cfg(test)] mod tests` blocks. Run as part of
     `nix flake check`.
   - **Integration tests** (`cargo nextest run --test`) — cross-crate, use
     mock agent processes over real pipes, no containers. Live in
     `loom/crates/<crate>/tests/*.rs`. Run as part of `nix flake check`.
   - **Container smoke** (`nix run .#test-loom`) — one happy-path scenario
     that spawns a real podman container via `wrapix run-bead`, runs a mock
     agent *inside* the container, drives `loom run --once` against it, and
     asserts the bead closes. Validates host↔container plumbing
     (entrypoint.sh, bind mounts, `WRAPIX_AGENT` branching, container
     teardown) — *not* protocol depth, which the integration tier already
     covers. Linux-only (no podman in Darwin CI).

2. **Mock agent processes** — process-level fixtures driven over real pipes
   from cargo integration tests, plus the in-container smoke:
   - **Mock pi** (`tests/loom/mock-pi/pi.sh`) — narrowly scoped scenario
     modes that exercise the *pipe-level* paths the parser unit tests
     can't reach (probe round-trip, prompt ack, mid-session steer,
     compaction re-pin via steer, `set_model` from phase config, plus
     `happy-path` for the container smoke).
   - **Mock claude** (`tests/loom/mock-claude/claude.sh`) — modes for
     mid-session steering via stream-json user message, the shutdown
     watchdog SIGTERM→SIGKILL escalation, plus `happy-path` for the
     container smoke.
   - **Out of scope for mocks**: tool-call simulation, malformed-JSONL
     injection, hang/timeout simulation, multi-turn — the parser unit
     tests cover these with inline string literals, where regressions
     are easier to read in PR diffs and fixtures don't bit-rot when
     pi/claude release new event shapes.

3. **Unit test coverage by crate** — every crate has inline
   `#[cfg(test)] mod tests` blocks plus integration tests under
   `tests/*.rs`. The lists below are the contract surfaces, not an
   exhaustive enumeration; specific edge cases live in the test code.

   #### loom-core
   - Newtype construction and serde round-trips (`BeadId`, `SpecLabel`,
     `MoleculeId`, `ProfileName`)
   - `StateDb` schema creation on first open
   - `StateDb` query methods return typed rows (`SpecRow`, `MoleculeRow`)
   - `StateDb::rebuild` populates from spec files and mock `bd` output
   - Companion-section parser: spec with a `## Companions` section
     containing two backtick-delimited paths yields two `companions` rows;
     spec without the section yields zero
   - Parser ignores: text outside backticks on a bullet line, blank
     bullets, multi-path bullets (skipped with warn, not error)
   - Parser is case-sensitive on the heading: `## companions` (lowercase)
     and `## Companion paths` are not recognized
   - `current_spec` / `set_current_spec` round-trip
   - `increment_iteration` returns updated count, starts at 0
   - `bd` CLI output parsing (JSON → typed structs)
   - `bd` CLI error mapping (exit codes → error variants)
   - `bd` CLI wrapper passes every argument via `Command::arg()` — never
     shell interpolation. Tests inject values containing shell
     metacharacters (`; rm -rf /`, `` `id` ``, `$(whoami)`) and assert
     they reach `bd` literally as one argv element each, never expanded
   - Config file loading (TOML parsing into `LoomConfig`), defaults when
     file is absent or fields are missing
   - `SpawnConfig` JSON serialization round-trips with stable field ordering
     and key names (the contract with `wrapix run-bead --spawn-config`).
     Adding a field is non-breaking; renaming or removing one is — the test
     pins the on-disk shape so changes surface as test failures, not silent
     wire-format drift. Includes the optional
     `model: Option<ModelSelection>` field with
     `#[serde(skip_serializing_if = "Option::is_none")]` so wrappers built
     before the field landed continue to round-trip identically

   #### loom-agent
   - Pi RPC command serialization (Rust struct → JSONL line)
   - Pi RPC event deserialization via two-phase strategy:
     - Envelope parse (`PiEnvelope` with `type` + `id`) classifies the line
     - Full parse into `PiResponse`, `PiEvent`, or `PiUiRequest`
     - Test that envelope-only parse does not fail on unknown fields
   - `PiResponse` success/failure discrimination: `success: true` extracts
     `data`, `success: false` extracts `error` message
   - `message_update` nested delta dispatch: `text_delta` →
     `AgentEvent::MessageDelta`, `error` → `AgentEvent::Error`,
     `thinking_delta` / `done` / toolcall deltas → skipped (empty events)
   - Pi `tool_execution_start` field mapping: `toolCallId` → `ToolCallId`,
     `toolName` → `tool`, `args` → `params`
   - Pi `tool_execution_end` field mapping: `result` → `output`,
     `isError` → `is_error`
   - Claude stream-json event deserialization (`#[serde(tag = "type")]` →
     `ClaudeMessage`)
   - Claude `#[serde(other)]` catches unknown event types without error
   - Per-phase backend resolution (`[agent.todo]` overrides `[agent] default`,
     `--agent` flag overrides all phases)
   - Malformed JSONL handling — specific test cases:
     - Truncated JSON (`{"type": "message_del`) → `ProtocolError::InvalidJson`
     - Valid JSON, wrong shape (`{"foo": 42}`) → `ProtocolError::UnknownMessageType`
     - Empty line between objects → silently skipped
     - Line containing only whitespace → silently skipped
     - Escaped `\n` inside a JSON string value (e.g. `{"text":"line1\nline2"}`)
       → parsed as a single line, string value contains literal newline
     - U+2028/U+2029 inside JSON string → passes through, not treated as
       line terminator
     - Trailing `\r\n` → `\r` stripped, parsed normally
     - Line exceeding `MAX_LINE_BYTES` (10 MB) → `ProtocolError::LineTooLong`
   - `ParsedLine::response` populated for Claude `control_request` (parser
     returns auto-approve JSON string, `events` is empty)
   - `ParsedLine::events` contains two events for Claude `result/success`
     (`TurnEnd` + `SessionComplete`); two events for `result/error`
     (`Error` + `SessionComplete`); Pi's `turn_end` and `agent_end` each
     map to a single event
   - `ParsedLine::response` is `None` for Pi events and Claude non-control
     events
   - Event normalization (both backends produce identical `AgentEvent`
     sequences for equivalent agent behavior)
   - Timeout behavior: no JSONL line for 5+ minutes → warning logged, no
     abort

   #### loom-templates
   - All templates compile — Askama enforces this at build time; an
     explicit `cargo nextest run -p loom-templates` is the regression
     gate
   - Template rendering with representative inputs produces output
     containing required partials, agent-output wrapping, and applied
     truncation (see *Architecture / Test Patterns / Template render
     contract*)
   - Layout regressions caught by `insta` snapshots (see
     *Architecture / Snapshot Testing*)
   - Partial inclusion works (context pinning, exit signals, spec
     header, companions, implementation notes)

   #### loom-workflow
   - Spec resolution logic (`--spec` flag, `current_spec` DB fallback, missing
     spec error)
   - Profile selection from bead labels (parse, fallback to base, flag
     override)
   - Retry logic (failure count tracking, `loom:clarify` label after max
     retries)
   - Push gate logic (clean completion, fix-up beads, iteration cap)
   - Four-tier detection (git diff, molecule-based, README discovery, new)
   - No per-bead `bd dolt push/pull` is invoked: assert `BdClient` exposes
     no `dolt_push`/`dolt_pull` methods and the workflow paths do not
     spawn `bd dolt …` subprocess calls (containers reach the authoritative
     state via the bind-mounted Dolt socket)
   - Parallel batch dispatch: given 3 ready beads and `--parallel 3`,
     the dispatcher creates 3 worktrees, spawns 3 `wrapix run-bead`
     futures concurrently, and reports all results before merge-back
   - Parallel batch with N=1 (the default): no worktree is created;
     work runs on the driver branch directly
   - Merge-back ordering: branches merge to the driver branch sequentially,
     not in parallel (avoids index lock races)
   - On worker failure, the bead's worktree branch is deleted and the bead
     is queued for retry per the retry policy
   - On merge conflict, the worktree path is preserved and the bead is
     marked failed (does not silently overwrite or auto-resolve)

   #### Concurrency & locking (loom-core)
   - `flock` wrapper acquires/releases an exclusive lock on a file path;
     blocking variant returns when the lock is free, try-variant returns a
     typed error if held
   - Per-spec lock path resolution: `.wrapix/loom/locks/<label>.lock` is
     created on first acquire, parent dirs created on demand
   - Workspace lock path: `.wrapix/loom/locks/workspace.lock`
   - Lock-class dispatch: `LockClass::None`, `LockClass::Spec(label)`,
     `LockClass::Workspace` are derived from the parsed CLI command before
     any side effects
   - Two threads contending on the same per-spec lock: first wins, second
     waits up to 5s then errors with a clear message naming the held label
   - Crash test: spawn a child, have it acquire the lock, kill it; parent
     re-acquires immediately (kernel released the flock)

   #### Auxiliary commands (loom-workflow)
   - `loom init` writes a default `config.toml` and creates `state.db` with
     the expected schema (specs, molecules, companions, meta tables)
   - `loom init` is idempotent: running twice does not clobber existing
     `current_spec` or molecule rows
   - `loom init --rebuild` drops and repopulates: spec rows from `specs/*.md`,
     molecule rows from mock `bd list --label=loom:active`, iteration
     counters reset to 0
   - `loom status` prints `current_spec`, active molecule id, iteration count
     in a stable format (parseable line-by-line)
   - `loom status` with no `current_spec` set exits 0 with a clear message,
     not an error
   - `loom use <label>` writes `current_spec` to the meta table; subsequent
     `loom status` reflects the change
   - `loom use <unknown>` errors when the spec row is missing
   - `loom logs` (no flags) prints the path of the most recent file
     under `.wrapix/loom/logs/` and tails it
   - `loom logs --bead <id>` finds the most recent log for that bead;
     exits non-zero with a stderr message naming the bead id when no
     log exists
   - `loom spec --deps` parses the active spec's `[verify]` / `[judge]`
     annotations, opens each referenced test file, and prints the
     deduplicated set of nixpkgs needed (port of `ralph sync --deps`)
   - CLI surface: `loom --help` lists every v1 command (`plan`,
     `todo`, `run`, `check`, `msg`, `spec`, `init`, `status`, `use`,
     `logs`) and does NOT list `sync`, `tune`, or `watch`

   #### Run UX renderer (loom-workflow)
   - Default mode: header line per bead, one line per tool call, no
     streamed assistant text deltas
   - `--verbose` / `-v` streams assistant text as it arrives
   - Tool call rendering: tool name + truncated single-line summary; lines
     longer than terminal width are clipped, never wrapped
   - Status colors: green for `✓ done`, red for `✗ failed`, yellow for
     retry; disabled when stdout is not a TTY (`NO_COLOR` honored)
   - Parallel mode: tool-call lines are bead-id-prefixed so interleaved
     output stays attributable

   #### Run logger (loom-workflow)
   - Every bead spawn produces a file at
     `.wrapix/loom/logs/<spec-label>/<bead-id>-<timestamp>.jsonl` containing
     the raw event stream as one JSON object per line
   - File is written for every spawn, regardless of terminal verbosity.
     The renderer and the on-disk logger both subscribe to the same
     `AgentEvent` channel; tests verify by routing the channel into
     two collectors and asserting line-for-line equality between the
     collected event sequences
   - With `--parallel N`, two concurrent spawns produce two distinct files;
     no shared writer, no interleaving
   - Log file path is logged at `info!` when the spawn starts so users can
     `tail -f` it
   - Retention sweep on `loom run` startup: with `retention_days = 14`,
     files mtime'd 15+ days ago are deleted, files mtime'd today are kept
   - `retention_days = 0` is a no-op (no files deleted, no walk)
   - Best-effort: a single un-deletable file (read-only, locked) does not
     abort the sweep — remaining deletable files are still removed

   #### GitClient (loom-core)
   - `GitClient::create_worktree(label, bead_id)` creates a worktree under
     `.wrapix/worktree/<label>/<bead-id>/` on a fresh branch
     `loom/<label>/<bead-id>` from HEAD
   - `GitClient::list_worktrees` returns worktrees registered with the
     repo (parity with `git worktree list --porcelain`)
   - `GitClient::remove_worktree(path)` removes the worktree directory
     and its registration; idempotent if already removed
   - `GitClient::merge(branch)` merges a bead branch into the current
     branch; returns a typed `MergeResult` distinguishing fast-forward,
     non-conflicting merge commit, and conflict
   - `GitClient::status` reports working-tree changes against HEAD
   - Hybrid implementation: callers see only the typed Rust API.
     Whichever path is used internally (gix vs `git` CLI) is
     encapsulated — no `gix::` or `tokio::process::Command::new("git")`
     references appear outside the `GitClient` module. Verified by
     `loom/crates/loom/tests/style.rs`.

4. **Integration test coverage** — load-bearing flows that exercise
   cross-crate behavior or pipe-level orchestration. Protocol-level
   shape coverage (event mapping, malformed JSONL, control_request
   responses) belongs in parser unit tests; this list is the
   integration-tier contract.
   - **Startup probe round-trip** — mock pi replies to `get_commands`
     with the full required set; loom proceeds. Mock pi replies with a
     missing required command; loom fails fast with a version-mismatch
     error.
   - **`wrapix run-bead` argv contract** — loom writes a `SpawnConfig`
     JSON, invokes a `wrapix-run-bead` shim that records the argv +
     stdin properties (TTY vs pipe), then exec's a mock agent. Asserts
     the JSON shape, the `--spawn-config <file> --stdio` argv, and that
     stdin is a pipe (not a TTY).
   - **Parallel run end-to-end** — `loom run --parallel 2` with two
     ready beads dispatches two mock-agent spawns concurrently
     (overlapping spawn timestamps captured by the mock), each in its
     own git worktree under `.wrapix/worktree/<label>/<bead-id>/`,
     then merges both branches back to the driver branch sequentially.
   - **`GitClient` round-trip** — create worktree, list, status,
     merge (clean / non-conflicting / conflict variants), remove —
     all against a temp repo via the typed Rust API.
   - **State DB lifecycle** — `StateDb::open` on a fresh path creates
     schema; `rebuild` populates from `specs/*.md` plus mock `bd`
     output; `recreate` recovers from a corrupted file.
   - **Per-spec advisory locking** — two contending acquisitions on
     the same `<label>.lock` serialize via `flock`; the second waits
     up to 5s (driven by `MockClock`), then errors with a clear
     message naming the held label. Crash test: child acquires + is
     killed; parent re-acquires immediately.
   - **Logging tee** — renderer and on-disk `.jsonl` log subscribe to
     the same `AgentEvent` stream; assert line-for-line equality on
     the log side.

5. **Container smoke coverage** — one happy-path scenario validates
   host↔container plumbing that the integration tier cannot reach: a
   temp `.beads/` is seeded with one ready bead labelled
   `profile:base`; a test image bundles `mock-pi` at a known path
   inside the container; loom invokes `wrapix run-bead` with
   `WRAPIX_AGENT=pi` and `MOCK_PI_SCENARIO=happy-path`; the smoke
   asserts the container exits clean and the bead closes.
   Workflow-level coverage (plan/todo/run/check/msg, profile selection,
   agent switching, runtime composition) lives in inline
   `#[cfg(test)] mod tests` blocks under `loom-workflow/src/` — those
   are exercised via `cargo nextest run`, not the smoke.

6. **Rust style enforcement** — two complementary mechanisms:
   - **Clippy lints** in `[workspace.lints.clippy]`: `unwrap_used = "deny"`,
     `expect_used = "deny"`, `panic = "deny"`, `todo = "deny"`,
     `unimplemented = "deny"`, `allow_attributes = "warn"`. Tests opt out
     via per-file `#![allow(clippy::unwrap_used, ...)]` at the top of
     `loom/crates/*/tests/*.rs` and inside `#[cfg(test)] mod tests` blocks.
   - **Source-walking checks** in `loom/crates/loom/tests/style.rs`
     for rules clippy can't express. Uses `syn` for AST patterns
     (no `derive(From)` / `derive(Into)` on tuple structs, `GitClient`
     encapsulation, single `AgentEvent` channel for renderer + log
     writer, newtype identifier shape, typed Askama context structs)
     and `walkdir` for filesystem-shape rules (no
     `loom/crates/*/src/{types,error}.rs` at crate roots).

7. **Annotation contract** — every acceptance criterion in any spec
   under `specs/` carries a `[verify]` or `[judge]` annotation that
   must resolve to an existing test function. The full rules
   (cardinality, classification, cross-spec sharing) and the CI gate
   that enforces them are defined in *Architecture / Annotation
   Contract* and *Architecture / Annotation Integrity Gate*.

8. **Property-based testing** — `proptest` for invariants on four
   targets: JSONL line parser, Pi protocol parser, Claude protocol
   parser, state DB rebuild. Properties target invariants ("never
   panics on arbitrary input", "round-trip is identity for known
   shapes", "unknown shapes map to typed errors") rather than
   specific input/output pairs. CI runs each property at
   `PROPTEST_CASES=32`; local exhaustive runs use `PROPTEST_CASES=2048+`
   via env var. No `cargo fuzz` under `nix flake check` — exposed
   separately as `nix run .#fuzz-loom` for on-demand or nightly use.

9. **Snapshot testing** — `insta` snapshots for templates and CLI help
    output (contract surfaces where layout regressions matter).
    Substring + structural assertions for the run-time renderer
    (terminal tool-call lines, status colors — surfaces with
    intentional flexibility). Snapshot updates require explicit
    acknowledgment in the PR description ("snapshot updated because:
    ...") to surface accidental drift.

### Non-Functional

1. **Deterministic** — no real LLM API calls; no real wall-clock waits.
   Mock agents return canned responses. Time-dependent components take
   an injectable `Clock` trait; tests use a `MockClock` with controllable
   advance (see *Architecture / Determinism Through Clock Injection*).
2. **Fast** — warm-cache `cargo nextest run --workspace` targets <5s
   on Linux CI; container smoke targets <30s. Compile time is
   separate from the test budget. Both targets guide design choices
   (see *Out of Scope / Hard CI-time NFR*).
3. **Isolated** — each test uses its own temp directory and beads database
   prefix. No shared mutable state between tests.
4. **Parallel-safe** — unit and integration tests run in parallel
   under `cargo nextest`'s process-per-test model. Each test gets a
   fresh process, so global state (env vars, working directory,
   process-level locks) doesn't leak between tests. The container
   smoke (single scenario) gets its own pre-seeded `.beads/` snapshot
   in a tempdir, fully isolated from any concurrent peers running
   against the workspace.
5. **CI-friendly** — `nix flake check` runs unit + integration tests
   via a single Nix derivation that invokes `cargo nextest run
   --workspace`. The container smoke is exposed as a separate `nix run
   .#test-loom` app because it needs podman at runtime.
6. **Real bd** — the container smoke runs against live `bd` (not a mock),
   consistent with ralph-tests.md's approach.
7. **Cross-platform** — unit and integration tests pass on Linux *and*
   Darwin (`x86_64`/`aarch64` for both). The container smoke is
   Linux-only (podman dependency); on Darwin the `test-loom` app exits
   0 with a clear "container smoke not available on Darwin" message.
   Tests use `tempfile::tempdir` exclusively, never hardcoded
   `/tmp/...` paths — Nix's Darwin build sandbox doesn't grant access
   to the host's `/tmp`, so any test that hardcodes one fails to even
   start under `nix flake check`. Darwin smoke support is a follow-up.
8. **Subprocess-spawning tests are exceptional** — each subprocess test
   (mock-pi, mock-claude, real `git`) costs 50-200ms; ten of them blow
   the 5s soft target alone. A test that spawns a subprocess must
   include a short comment or doc string explaining why an in-process
   equivalent (via `LineParse` + `tokio::io::duplex`) isn't feasible.
9. **Upstream protocol versioning** — pi-mono and Claude Code versions
   are pinned in `modules/flake/overlays.nix`. Bumps are deliberate PRs
   accompanied by a protocol-bump checklist (re-run parser tests, scan
   upstream changelog for new event types, add `Unknown` coverage if
   any new types lack typed variants, update mock scripts if new types
   reach pipe-level paths). No live wire tests against real binaries.
   Detection coverage: silent breaks in *exercised* fields surface as
   `serde_json` errors in parser tests when the pinned version is
   bumped. Fields not exercised by any test could still drift silently
   — parser tests must therefore touch every field of every documented
   message type for the pinned version, not just every type.
10. **No `#[ignore]` for flake mitigation** — a test marked `#[ignore]`
    because "it flakes sometimes" is forbidden. Either fix the root
    cause or delete the test. `#[ignore]` is reserved for tests that
    require explicit opt-in (e.g., the container smoke needing podman).
    A CI flake opens a `loom-flake` P1 bead naming the failing test;
    the test is fixed before any further work on the affected crate.

## Architecture

### Test File Layout

Each crate uses two complementary Rust test homes:

- **Inline `#[cfg(test)] mod tests { … }`** at the bottom of each source file
  — white-box tests with access to private impl details, kept next to the
  code they exercise so changes land together.
- **Cargo integration tests** under `loom/crates/<crate>/tests/*.rs` —
  black-box tests that import the crate by its public API, exercising
  cross-module behaviour and the surfaces that downstream crates also see.

```
loom/
  crates/
    loom-core/
      src/
        state/
          db.rs               # StateDb impl + inline #[cfg(test)] mod tests
          rebuild.rs          # rebuild logic + inline tests
          companions.rs       # `## Companions` parser + inline tests
        bd/
          client.rs           # bd CLI wrapper + inline tests
          label.rs            # Label newtype + inline tests
        identifier/
          bead.rs             # BeadId + inline tests (validation, serde)
          ...                 # one file per id newtype, all with inline tests
        agent/
          repin.rs            # RePinContent + inline tests (doc-tested)
          ...
      tests/
        state_db.rs           # Integration: StateDb across rebuild + queries
        lock_manager.rs       # Integration: per-spec advisory locking
        git_client.rs         # Integration: GitClient against a temp repo
        logging.rs            # Integration: shared renderer + log channel
        properties.rs         # proptest invariants for state DB rebuild
    loom-agent/
      src/
        pi/
          mod.rs
          parser.rs           # JSONL parsing + inline tests (string literals)
          backend.rs          # spawn / lifecycle + inline tests driving mock-pi
          messages.rs
        claude/
          mod.rs
          parser.rs           # stream-json parsing + inline tests (string literals)
          backend.rs          # spawn / lifecycle + inline tests driving mock-claude
          messages.rs
      tests/
        static_dispatch.rs    # Compile-time check: both backends impl AgentBackend
        properties.rs         # proptest invariants for pi/claude protocol parsers
    loom-workflow/
      src/
        run/
          mod.rs              # run loop + inline tests for unit-level helpers
        check/
          mod.rs              # push gate + inline tests
        ...
      tests/
        parallel.rs           # Integration: --parallel N worktree dispatch
    loom-templates/
      src/
        ...                   # per-template module + inline rendering tests
      tests/
        render.rs             # Integration: every template renders with partials
    loom/
      tests/
        run_smoke.rs          # Integration: CLI subcommand surface
        agent_flag.rs         # Integration: --agent flag parsing/validation
        spawn_dispatch.rs     # Integration: shim-based wrapix run-bead argv
                              #   contract + stdin-pipe-not-tty assertion
        style.rs              # AST + filesystem style enforcement (syn-based)
        annotations.rs        # Annotation-integrity gate (walks specs/*.md)
        properties.rs         # proptest invariants (pi/claude parsers, state DB)

tests/
  loom-test.sh                # [verify] runner — invoked per-function by
                              #   `ralph spec --verify`; each function shells
                              #   to a specific cargo test
  loom/
    default.nix               # Nix derivation: cargo nextest run --workspace
    run-tests.sh              # Container smoke harness (single happy-path)
    mock-pi/pi.sh             # Mock pi (scoped scenario modes)
    mock-claude/claude.sh     # Mock claude (scoped scenario modes)
```

### Annotation Contract

Every acceptance criterion in any spec under `specs/` carries one
annotation in one of these forms:

```
- [ ] Criterion text
  [verify](path/to/file.sh::test_function_name)
- [ ] Criterion text
  [judge](path/to/file.sh::test_function_name)
```

**Rules:**

1. **Every annotation MUST resolve.** The named function exists in the
   named file. Enforced by a CI gate (see *Annotation Integrity Gate*
   below).
2. **`[verify]` vs `[judge]` is mechanical.** `[verify]` means a
   deterministic check (Rust test, AST walk, filesystem assertion,
   shell exit code). `[judge]` means a check that *requires* LLM
   evaluation — code-quality criteria like "error messages are
   actionable" or "naming is consistent." Anything reducible to AST
   patterns or filesystem state is `[verify]`, not `[judge]`.
   Loom-tests v1 has no `[judge]` criteria, and the integrity gate
   actively enforces this: any `[judge]` annotation in any spec is a
   hard error until the judge runner is set up. The mechanism for
   future use is documented in *Judge Mechanism*.
3. **Atomic acceptances.** One acceptance → exactly one annotation.
   Forbidden: one acceptance with two `[verify]` markers (ambiguous
   pass/fail when one passes and the other fails). If a criterion
   needs multiple tests, split it into multiple atomic criteria.
4. **N→1 sharing allowed.** Multiple acceptances may point to the same
   function when one test asserts multiple behaviors. The spec lists
   them as separate criteria because they're separately documented
   requirements.
5. **Cross-spec sharing allowed.** One function may be referenced from
   multiple specs.

### Annotation Integrity Gate

A bidirectional gate enforces the annotation contract:

**Forward direction** — every `[verify]` / `[judge]` annotation in
`specs/*.md` must resolve to an existing function in the named file:

- **Shell paths** (e.g., `tests/loom-test.sh::test_X`) — file must
  exist; the regex `^test_X\(\)` must match a function definition.
- **Cargo paths** — the gate doesn't transitively follow cargo
  invocations. The shell function must exist; *its body* is the
  contract. If the shell function shells to a non-existent cargo
  test, that surfaces when CI invokes it.

**Reverse direction** — every top-level zero-argument function in
`tests/loom-test.sh` whose name starts with `test_` must be
referenced by at least one annotation in some spec. Helper functions
named `_helper`, `_setup`, etc. are exempt by the naming rule.
Stale verify functions are a code smell: either the criterion is
missing from a spec, the function name is wrong, or the function
should be deleted.

**Implementation**: `loom/crates/loom/tests/annotations.rs` walks
`specs/*.md`, regex-extracts annotations, asserts each function
resolves, and asserts every shell-runner function is referenced.
Output on failure: `<spec>:<line>: annotation [verify](...) — function
not found`. Runs under `nix flake check`.

The gate verifies itself: this spec's acceptance criterion for the
gate carries
`[verify](tests/loom-test.sh::test_acceptance_annotations_resolve)`,
which resolves to the gate's implementation. Each CI run therefore
includes the gate's own integrity check.

**Scope**: walks `specs/*.md` for annotations; only validates
annotations pointing into `tests/loom-test.sh`. Other test runners
(e.g., a future `tests/sandbox-test.sh`) own their own annotation
gates. Per Annotation Contract rule 2, any `[judge]` annotation
encountered today fails the gate; when the judge runner is set up
(at `tests/judges/loom.sh` or equivalent), the gate's resolution
logic extends to that path.

### Determinism Through Clock Injection

Time-dependent components — lock acquisition timeout, shutdown
watchdog grace, JSONL read-line timeout, log retention sweep, bd /
git subprocess timeouts — make tests flaky when they touch real wall
time on a loaded CI runner. The design eliminates real-time waits.

**`Clock` trait in `loom-core`** with `now()`, `sleep(Duration)`,
`timeout(Duration, Future)` async surface. Two implementations:

- `SystemClock` — production. Wraps tokio's real timers.
- `MockClock` — tests. Deterministic advance under
  `#[tokio::test(start_paused = true)]`.

Components touching time take `&dyn Clock` or `<C: Clock>`. Functions
comparing against external timestamps (e.g., the log retention sweep
comparing against filesystem mtime) take `now: Instant` as a
parameter. Tests pass synthetic `now` values to age files; production
passes `clock.now()`.

**Filesystem mtime in tests** is set via the `filetime` crate. Real
wall time stays zero; tests can express "this file is 15 days old"
without sleeping.

**Banned patterns** (enforced by `loom/crates/loom/tests/style.rs`
AST check):

- `std::thread::sleep` — anywhere, no exceptions.
- `tokio::time::sleep` outside `SystemClock::sleep`'s implementation.
- `tokio::time::timeout` outside `SystemClock::timeout`'s
  implementation.
- `Instant::now()` / `SystemTime::now()` outside `SystemClock::now()`.

Tests that need to advance time construct a `MockClock` directly
(`MockClock::new()` or via a small `with_mock_clock` helper in
`loom-core::testing`) and pass it as `&dyn Clock` into whatever
component is under test. There is no other opt-out path; the bans
apply uniformly across `src/` and `tests/`.

### Style Enforcement

Two complementary mechanisms in
`loom/crates/loom/tests/style.rs`:

**Workspace clippy lints** for what clippy supports natively:

| Rule | Lint |
|------|------|
| no `unwrap()` | `clippy::unwrap_used = "deny"` |
| no `expect()` | `clippy::expect_used = "deny"` |
| no `panic!()` | `clippy::panic = "deny"` |
| no `todo!()` | `clippy::todo = "deny"` |
| no `unimplemented!()` | `clippy::unimplemented = "deny"` |
| no `#[allow(dead_code)]` (use `#[expect]`) | `clippy::allow_attributes = "warn"` |

Tests opt out via per-file `#![allow(clippy::unwrap_used, ...)]` at
the top of `loom/crates/*/tests/*.rs` and inside `#[cfg(test)] mod
tests` blocks. Already the convention in the workspace.

**Source-walking checks** for rules clippy can't express. One test
covers both style rules and architectural assertions, using `syn` for
AST patterns and `walkdir` for filesystem-shape rules:

| Rule | Mechanism |
|------|-----------|
| no `derive(From)` / `derive(Into)` on tuple structs | `syn::ItemStruct` walk over `loom/crates/*/src/**/*.rs` |
| no `loom/crates/*/src/{types,error}.rs` files | `walkdir` filter on `loom/crates/*/src/` |
| `GitClient` is the only `gix` / `git` CLI importer | `syn` walk asserting `use gix::*` and `Command::new("git")` appear only in `loom-core/src/git/` |
| renderer + log writer share one `AgentEvent` channel | `syn` walk asserting both subscribe to the same `tokio::sync::broadcast` (or `mpsc`) sender |
| domain identifiers are tuple-struct newtypes | `syn::ItemStruct` walk over `loom-core/src/identifier/` |
| each Askama template has a typed context struct | `syn` walk pairing `#[derive(Template)]` structs in `loom-templates/src/` with their `templates/*.md` files |
| tests use `tempfile::tempdir`, never hardcoded `/tmp/...` | `syn` literal walk over `loom/crates/*/tests/**/*.rs` and `#[cfg(test)]` blocks |

`syn` and `walkdir` are `[dev-dependencies]` of the binary crate
(no propagation to dependents). Output on failure: `<path>:<line>
<rule>` so reviewers can click directly into the violation.

### Property-Based Testing

`proptest` for invariants on four targets:

| Target | Invariants |
|--------|------------|
| JSONL line parser | never panics on arbitrary bytes; respects `MAX_LINE_BYTES`; never emits `AgentEvent` from a malformed line |
| Pi protocol parser | round-trip identity for known shapes; unknown shapes map to `ProtocolError::UnknownMessageType`; never panics |
| Claude protocol parser | round-trip identity for known shapes; unknown shapes map to the `Unknown` variant via `#[serde(other)]`; never panics |
| State DB rebuild | never panics on arbitrary spec file content; schema invariants always hold; corrupted DB always recovers via `recreate` |

**CI configuration**: `PROPTEST_CASES=32` for `nix flake check`,
overridable via env var to `2048+` for local exhaustive runs.
Property tests live in `loom/crates/<crate>/tests/properties.rs` —
each crate owns the invariants for the types it defines. The binary
crate's `loom/crates/loom/tests/properties.rs` is reserved for
cross-crate invariants if any arise.

**No `cargo fuzz` under `nix flake check`.** If a fuzz target later
proves valuable for byte-level edge cases proptest misses (e.g.,
JSONL framing under adversarial input), it's exposed as
`nix run .#fuzz-loom` for on-demand or nightly use, never gating PRs.

### Snapshot Testing

`insta` snapshots for **contract surfaces** — outputs whose shape is
the contract:

- Templates (`loom-templates`) — every Askama template × representative
  input set produces a `.snap` checked into
  `loom/crates/loom-templates/tests/snapshots/`. Reviewers see the
  rendered diff in PRs.
- CLI help text (`loom --help`, `loom run --help`, etc.) — `--help`
  output *is* the user contract.

Substring + structural assertions for **flexibility surfaces** —
outputs with intentional cosmetic latitude:

- Run-time renderer (terminal tool-call lines, status colors,
  truncation). Tests assert bullet count, presence of key markers,
  and color-disabled-when-NO_COLOR; layout decisions remain free to
  evolve without churning a snapshot.

**Snapshot update policy**: a snapshot diff in a PR requires explicit
acknowledgment in the PR description ("snapshot updated because:
..."). Forces intentional regression vs. accidental drift.

### Judge Mechanism

`[judge]` annotations are reserved for criteria that genuinely require
LLM evaluation — code-quality dimensions that AST walks can't capture:

- "error messages are clear and actionable"
- "doc comments explain *why* non-obviously"
- "API surface is ergonomic for typical call patterns"
- "naming is consistent with codebase conventions"

Loom-tests v1 has no `[judge]` criteria; the mechanism below is
documented for future use.

**Runner**: separate from `cargo test`. Invocation:
`loom judge run --annotation <spec.md::criterion>` (or `bd judge` once
that command exists). The runner sends the named source files plus the
criterion text to Claude via the existing agent abstraction and
captures a structured verdict.

**Output**: a bead comment with the verdict. Advisory only — judges
do not gate CI.

**Why advisory**: judges are non-deterministic, paid, and
network-dependent. They do NOT run under `nix flake check`; they run
on demand or in a nightly job. A `[judge]` verdict that disagrees with
human judgement is a prompt to either rewrite the criterion as
`[verify]` (if the property is reducible to a structural check) or
accept the disagreement (if the property is genuinely subjective).

### Test Patterns

Concrete patterns for writing tests against the design rules above. Each
pattern is one short example; the verify-runner and integration tests
own the full coverage.

#### Parse, Don't Validate boundaries

Each boundary layer pins the parse-once-use-everywhere contract with
a dedicated test. Three illustrative examples below — newtype
construction, two-phase envelope parsing, and `#[serde(other)]`
catchall behavior. JSONL framing and SQLite row mapping have
analogous tests in `loom-core/src/{agent,state}` that follow the
same shape:

```rust
#[test]
fn newtype_roundtrip() {
    let id = BeadId::new("wx-abc123").unwrap();
    assert_eq!(id.as_str(), "wx-abc123");
    assert_eq!(id.to_string(), "wx-abc123");

    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, r#""wx-abc123""#); // transparent, no wrapper
    let parsed: BeadId = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, id);

    // Deserialize validates the canonical shape.
    serde_json::from_str::<BeadId>(r#""not a bead""#).unwrap_err();
}

#[test]
fn pi_envelope_ignores_unknown_fields() {
    // Two-phase: envelope parse must succeed even with extra fields
    let line = r#"{"type":"response","id":"42","extra":"ignored"}"#;
    let env: PiEnvelope = serde_json::from_str(line).unwrap();
    assert_eq!(env.msg_type.as_deref(), Some("response"));
}

#[test]
fn claude_unknown_event_type_does_not_error() {
    // #[serde(other)] catches new event types from future Claude versions
    let line = r#"{"type":"new_feature_event","data":"something"}"#;
    let msg: ClaudeMessage = serde_json::from_str(line).unwrap();
    assert!(matches!(msg, ClaudeMessage::Unknown));
}
```

#### State database

Round-trip and corruption-recovery tests use real on-disk SQLite
files inside `tempfile::tempdir`. The `:memory:` mode is deliberately
not used — it skips the file-IO codepaths that production runs hit
(open, fsync, corruption recovery), so an in-memory test passing
gives false confidence.

```rust
#[test]
fn state_db_rebuild() {
    let dir = tempdir().unwrap();

    // Seed spec files
    std::fs::create_dir_all(dir.path().join("specs")).unwrap();
    std::fs::write(dir.path().join("specs/auth.md"), "# Auth\n").unwrap();
    std::fs::write(dir.path().join("specs/api.md"), "# API\n").unwrap();

    let db = StateDb::open(&dir.path().join("state.db")).unwrap();
    let report = db.rebuild(dir.path(), &mock_bd_client()).unwrap();

    assert_eq!(report.specs_found, 2);
    assert!(report.counters_reset);

    let spec = db.spec(&SpecLabel::new("auth")).unwrap();
    assert_eq!(spec.spec_path, "specs/auth.md");
}

#[test]
fn state_db_corruption_recovery() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("state.db");

    // Write garbage to the DB file
    std::fs::write(&db_path, b"not a sqlite db").unwrap();

    // open detects corruption, rebuild recovers
    let db = StateDb::open(&db_path).unwrap();
    let report = db.rebuild(dir.path(), &mock_bd_client()).unwrap();
    assert_eq!(report.specs_found, 0); // no spec files in tempdir
}
```

#### Template render contract

Render tests assert on the contract (partials included, agent content
wrapped, truncation applied) rather than full string parity. Contract
shape comes from the typed `RunContext` struct; layout regressions are
caught by `insta` snapshots (see *Snapshot Testing*).

```rust
#[test]
fn run_wraps_agent_supplied_fields_in_agent_output() -> Result<()> {
    let ctx = RunContext {
        pinned_context: PINNED_CONTEXT_BODY.into(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".into(),
        issue_id: BeadId::new("wx-3hhwq.6")?,
        title: "Implement parser".into(),
        description: "agent-supplied body".into(),
        previous_failure: None,
        // ...
    };
    let out = ctx.render()?;

    assert!(out.contains("<agent-output>"));
    assert!(out.contains("</agent-output>"));
    assert!(out.contains("agent-supplied body"));
    Ok(())
}
```

### Mock Pi Design

Mock pi is a shell script that frames pi-mono's RPC protocol as JSONL
on stdin/stdout. Its job is to exercise *process-level* paths the
parser unit tests cannot reach — round-tripping through real pipes,
stdin write-back from `ParsedLine::response`, and child reaping. Each
mode is shaped to exactly one Rust test that drives it; the script is
not a general-purpose pi emulator.

Modes (selected via `argv[1]`):

| Mode | Used by | Wire behavior |
|------|---------|---------------|
| `probe-ok` | startup probe round-trip test | Replies to `get_commands` with the full required set |
| `probe-missing-set-model` | startup probe failure test | Replies to `get_commands` omitting `set_model` |
| `echo-prompt` | wire-shape assertion test | Probe ok, then echoes the prompt payload as a `message_delta` |
| `steering` | mid-session steer test | Probe ok, prompt → first turn, then echoes the steer payload on the next turn |
| `compaction` | re-pin-via-steer test | Probe ok, emits `compaction_start`, expects the re-pin steer, echoes it back, emits `compaction_end` |
| `set-model` | per-phase model override test | Probe ok, expects `set_model { provider, modelId }`, echoes the pair into a later `message_delta` |
| `happy-path` | container smoke | Probe ok, prompt → `message_delta` → `agent_end` |

Each mode is single-shot: the script runs until the conversation it
encodes completes, then exits. The Rust test owns the assertions; the
mock owns the wire framing.

### Mock Claude Design

Mock claude follows the same pattern as mock pi but speaks Claude
Code's stream-json framing (also JSONL) on stdin/stdout.

| Mode | Used by | Wire behavior |
|------|---------|---------------|
| `steering` | mid-session steer test | Emits one assistant turn, waits for a stream-json user message on stdin, emits a second assistant turn echoing the steer payload, then `result/success` |
| `ignore-stdin` | shutdown watchdog test | Emits `result/success`, ignores SIGTERM and stdin close so the test exercises the SIGTERM → SIGKILL escalation |
| `happy-path` | container smoke | system → assistant → `result/success` |

### Nix Integration

```nix
# tests/loom/default.nix
{ pkgs, loom, bd, ... }:
{
  # Unit + integration tests — run under `nix flake check`.
  loom-tests = pkgs.runCommandLocal "loom-tests" {
    nativeBuildInputs = [ loom.cargoDeps loom.rustToolchain pkgs.cargo-nextest ];
  } ''
    cd ${loom.src}
    cargo nextest run --workspace
    mkdir -p $out
  '';

  # Container smoke — invoked via `nix run .#test-loom`. Excluded from
  # `flake check` because it needs podman at runtime.
  loom-smoke = pkgs.writeShellApplication {
    name = "test-loom";
    runtimeInputs = [ loom bd pkgs.podman pkgs.jq ];
    text = builtins.readFile ./run-tests.sh;
  };
}
```

`loom-tests` is included in the checks set exposed by `tests/default.nix`
so it gates PRs in CI. `loom-smoke` is exposed as an app on Linux only.

## Affected Files

### New

| File | Role |
|------|------|
| `tests/loom-test.sh` | Per-function `[verify]` runner — each function shells to a specific cargo test |
| `loom/crates/*/src/**/*.rs` | Source files carry inline `#[cfg(test)] mod tests` blocks |
| `loom/crates/*/tests/*.rs` | Per-crate cargo integration tests |
| `loom/crates/loom/tests/style.rs` | Clippy + `syn`-based AST + filesystem style enforcement |
| `loom/crates/loom/tests/annotations.rs` | Bidirectional annotation-integrity gate |
| `loom/crates/<crate>/tests/properties.rs` | Per-crate `proptest` invariants — `loom-core` for state DB, `loom-agent` for protocol parsers; binary-crate `loom/crates/loom/tests/properties.rs` reserved for future cross-crate invariants |
| `loom/crates/loom-templates/tests/snapshots/` | `insta` snapshot files for templates |
| `tests/loom/default.nix` | Nix derivation: `cargo nextest run --workspace` + container smoke app |
| `tests/loom/run-tests.sh` | Container smoke harness (single happy-path scenario) |
| `tests/loom/mock-pi/pi.sh` | Mock pi (scoped scenario modes) |
| `tests/loom/mock-claude/claude.sh` | Mock claude (scoped scenario modes) |

### Modified

| File | Change |
|------|--------|
| `tests/default.nix` | Wire `loom-tests` into the checks set |
| `flake.nix` | Expose `test-loom` app on Linux only; expose `fuzz-loom` app for on-demand fuzz runs |
| `loom/Cargo.toml` | Add `[workspace.lints.clippy]` denying `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`; warning `allow_attributes` |
| `loom/crates/loom/Cargo.toml` | Add `[dev-dependencies]` for `syn`, `walkdir`, `filetime` (used by `style.rs`, `annotations.rs`, `properties.rs`) |
| `loom/crates/loom-core/Cargo.toml` | Add `[dev-dependencies]` for `proptest`, `tokio-test` (used by `MockClock` and per-crate property tests) |
| `loom/crates/loom-templates/Cargo.toml` | Add `[dev-dependencies]` for `insta` |
| `loom/crates/loom-core/src/` | Add `Clock` trait + `SystemClock` / `MockClock` impls |

## Success Criteria

### Unit tests

- [ ] Newtype serde round-trip tests cover all ID types (`BeadId`,
      `SpecLabel`, `MoleculeId`, `ProfileName`, `SessionId`,
      `ToolCallId`, `RequestId`)
  [verify](tests/loom-test.sh::test_newtype_serde_roundtrip)
- [ ] State database round-trip tests cover spec, molecule, and meta
      operations
  [verify](tests/loom-test.sh::test_state_db_roundtrip)
- [ ] Pi RPC protocol tests cover every command and every event type
      in the pi v0.72 protocol table, asserting on every documented
      field (not just type discrimination) so a renamed field fails
      deserialization at test time
  [verify](tests/loom-test.sh::test_pi_protocol_coverage)
- [ ] Claude stream-json protocol tests cover all `ClaudeMessage`
      variants including `Unknown` via `#[serde(other)]`, with
      field-level assertions on each variant
  [verify](tests/loom-test.sh::test_claude_protocol_coverage)
- [ ] Template rendering tests cover every Askama template with
      representative inputs
  [verify](tests/loom-test.sh::test_template_rendering)

### Integration tests

Each criterion below corresponds to one of the seven load-bearing flows
in Functional #4.

- [ ] Startup probe round-trip: mock pi with full required command set
      → loom proceeds; mock pi missing `set_model` → loom fails fast
      with a version-mismatch error
  [verify](tests/loom-test.sh::test_startup_probe_roundtrip)
- [ ] `wrapix run-bead` argv contract: loom invokes
      `wrapix run-bead --spawn-config <file> --stdio` with stdin
      attached as a pipe (not a TTY); recorded `SpawnConfig` JSON
      matches the on-disk shape
  [verify](tests/loom-test.sh::test_wrapix_run_bead_argv_contract)
- [ ] Parallel run end-to-end: `loom run --parallel 2` with two ready
      beads dispatches two mock-agent spawns concurrently, each in its
      own worktree, then merges both branches back to driver
  [verify](tests/loom-test.sh::test_parallel_run_end_to_end)
- [ ] `GitClient` round-trip: create worktree, list, status, merge
      (clean / non-conflicting / conflict variants), remove — all
      against a temp repo via the typed Rust API
  [verify](tests/loom-test.sh::test_git_client_roundtrip)
- [ ] State DB lifecycle: `open` on fresh path creates schema;
      `rebuild` populates from `specs/*.md` plus mock `bd` output,
      resetting iteration counters; `recreate` recovers from a
      corrupted file
  [verify](tests/loom-test.sh::test_state_db_lifecycle)
- [ ] Per-spec advisory locking: two contending acquisitions on the
      same `<label>.lock` serialize via `flock`; the second waits
      via `MockClock` advance, then errors naming the held label.
      Crashed child releases lock immediately for parent
  [verify](tests/loom-test.sh::test_per_spec_locking)
- [ ] Logging tee: renderer and on-disk `.jsonl` log subscribe to the
      same `AgentEvent` stream — capturing both yields line-for-line
      equality on the log side
  [verify](tests/loom-test.sh::test_logging_tee_equality)

### Container smoke

- [ ] `nix run .#test-loom` spawns a real podman container, runs
      `loom run --once` against a bead with `WRAPIX_AGENT=pi`
      `MOCK_PI_SCENARIO=happy-path`, exits 0 with the bead closed
  [verify](tests/loom-test.sh::test_loom_smoke_real_container)

### Style enforcement

**Configuration** — these tests check that the rules are *declared*:

- [ ] `[workspace.lints.clippy]` denies `unwrap_used`, `expect_used`,
      `panic`, `todo`, `unimplemented`; warns `allow_attributes`
  [verify](tests/loom-test.sh::test_workspace_clippy_lints)

**Outcome** — these tests check that the *codebase complies* with
the rules:

- [ ] `cargo clippy --workspace` passes (the gate that catches lint
      violations as they happen)
  [verify](tests/loom-test.sh::test_clippy_clean)
- [ ] No `derive(From)` / `derive(Into)` on tuple-struct newtypes
  [verify](tests/loom-test.sh::test_no_derive_from_on_newtypes)
- [ ] No `loom/crates/*/src/{types,error}.rs` files at crate roots
  [verify](tests/loom-test.sh::test_nested_module_structure)
- [ ] `GitClient` is the only module importing `gix` or invoking the
      `git` CLI
  [verify](tests/loom-test.sh::test_git_client_encapsulation)
- [ ] Renderer + log writer subscribe to the same `AgentEvent` channel
  [verify](tests/loom-test.sh::test_run_single_event_channel)
- [ ] Domain identifiers are tuple-struct newtypes
  [verify](tests/loom-test.sh::test_newtypes_for_identifiers)
- [ ] Each Askama template has a typed context struct
  [verify](tests/loom-test.sh::test_template_context_structs)
- [ ] Tests use `tempfile::tempdir`, never hardcoded `/tmp/...` paths
  [verify](tests/loom-test.sh::test_no_hardcoded_tmp_paths)

### Determinism

- [ ] No `std::thread::sleep` in any source file
  [verify](tests/loom-test.sh::test_no_thread_sleep)
- [ ] No `tokio::time::sleep` outside `SystemClock::sleep`
  [verify](tests/loom-test.sh::test_no_tokio_sleep_outside_clock)
- [ ] No `tokio::time::timeout` outside `SystemClock::timeout`
  [verify](tests/loom-test.sh::test_no_tokio_timeout_outside_clock)
- [ ] No `Instant::now()` / `SystemTime::now()` outside `SystemClock`
  [verify](tests/loom-test.sh::test_no_real_clock_outside_system_clock)
- [ ] No `#[ignore]` outside the container smoke runner
  [verify](tests/loom-test.sh::test_no_ignore_for_flake)

### Annotation gate

- [ ] Every `[verify]` / `[judge]` annotation in `specs/*.md` resolves
      to an existing function in the named file
  [verify](tests/loom-test.sh::test_acceptance_annotations_resolve)
- [ ] Every `test_*` function in `tests/loom-test.sh` is referenced by
      at least one annotation in some spec
  [verify](tests/loom-test.sh::test_no_orphan_test_functions)

### Property-based testing

- [ ] JSONL line parser proptest: never panics on arbitrary bytes,
      respects `MAX_LINE_BYTES`, never emits `AgentEvent` from a
      malformed line
  [verify](tests/loom-test.sh::test_jsonl_parser_invariants)
- [ ] Pi protocol parser proptest: round-trip identity for known
      shapes, unknown shapes map to typed errors, never panics
  [verify](tests/loom-test.sh::test_pi_parser_invariants)
- [ ] Claude stream-json parser proptest: round-trip identity for
      known shapes, `Unknown` variant catches unknown types, never
      panics
  [verify](tests/loom-test.sh::test_claude_parser_invariants)
- [ ] State DB rebuild proptest: arbitrary spec content never
      corrupts schema; corrupted DB always recovers via `recreate`
  [verify](tests/loom-test.sh::test_state_db_rebuild_invariants)
- [ ] `PROPTEST_CASES=32` for CI; overridable via env var
  [verify](tests/loom-test.sh::test_proptest_case_count)

### Snapshot testing

- [ ] Every Askama template has at least one `insta` snapshot under
      `loom/crates/loom-templates/tests/snapshots/`
  [verify](tests/loom-test.sh::test_template_snapshots_exist)
- [ ] `loom --help` and every subcommand `--help` have `insta`
      snapshots
  [verify](tests/loom-test.sh::test_cli_help_snapshots_exist)
- [ ] Run-time renderer uses substring + structural assertions, not
      `insta` (ensures terminal-output flexibility)
  [verify](tests/loom-test.sh::test_renderer_no_insta_dependency)

### Cross-platform

- [ ] `flake.nix` declares `loom-tests` under `checks.<system>` for
      `x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, `aarch64-darwin`
  [verify](tests/loom-test.sh::test_flake_declares_loom_for_all_systems)
- [ ] `nix run .#test-loom` is exposed only on Linux systems
  [verify](tests/loom-test.sh::test_smoke_linux_only)
- [ ] `nix run .#test-loom` on Darwin exits 0 with a clear "not
      available on Darwin" message
  [verify](tests/loom-test.sh::test_smoke_darwin_skip_message)

### CI integration

- [ ] `nix flake check` includes the loom-tests derivation that runs
      `cargo nextest run --workspace`
  [verify](tests/loom-test.sh::test_flake_check_includes_loom)
- [ ] `nix run .#test-loom` exists as a `writeShellApplication` exposed
      on Linux platforms
  [verify](tests/loom-test.sh::test_system_runner_exists)
- [ ] `nix run .#fuzz-loom` exists for on-demand `cargo fuzz` runs
      (not gated by `nix flake check`)
  [verify](tests/loom-test.sh::test_fuzz_runner_exists)
- [ ] Warm-cache `cargo nextest run --workspace` completes in <5s
      (soft target, not hard NFR)
  [verify](tests/loom-test.sh::test_cargo_nextest_timing)
- [ ] Container smoke completes in <30s
  [verify](tests/loom-test.sh::test_smoke_timing)
- [ ] pi-mono and Claude Code versions pinned in
      `modules/flake/overlays.nix`
  [verify](tests/loom-test.sh::test_protocol_versions_pinned)

## Out of Scope

- **Real-binary tests at any tier** — no test invokes real pi-mono,
  real Claude Code, or any LLM API. Mock pi and mock claude scripts
  cover the protocol surface (parser tests use inline strings; mocks
  cover pipe-level paths; smoke runs mock pi inside the container).
  Validation against real binaries happens during development, outside
  CI. Pinned versions in `modules/flake/overlays.nix` plus parser
  tests with field-level coverage catch silent protocol drift on
  bumps.
- **Ralph bash test migration** — ralph-tests.md tests remain as-is.
  They validate bash behavior; Loom tests validate Rust behavior. No
  migration is required.
- **macOS container smoke** — the smoke requires `podman` (Linux). Darwin
  container testing is a follow-up.
- **Mocking `bd`** — the container smoke uses live `bd`, consistent with
  ralph-tests.md (see NFR #6).
- **Broader system-tier scenario library** — `tests/loom/scenarios/` with
  steering, compaction, error-recovery scripts. The integration tier
  already covers these flows via shim-based mocks; repeating them with
  podman adds CI time without catching new failure modes. One happy-path
  smoke is sufficient to validate host↔container plumbing.
- **Captured JSONL fixtures** — `loom-agent/src/{pi,claude}/fixtures/`
  with replay scripts. Parser tests use inline string literals, which are
  easier to read in PR diffs and don't bit-rot when pi/claude release new
  event shapes.
- **Bash-rendered template parity fixtures** — Ralph's bash templates are
  slated for removal once Loom reaches parity, so capturing them as
  compatibility fixtures creates a fixture set that becomes irrelevant
  the moment Ralph is removed.
- **Pi cost capture** — deferred to loom-agent. When pi's
  `get_session_stats` is wired up after the startup probe, loom-tests
  gains one acceptance criterion: a round-trip test asserting that
  `SessionOutcome.cost_usd` is populated for pi sessions, parallel to
  the existing claude `result/total_cost_usd` extraction.
- **Mock-script protocol breadth** — tool-call simulation, malformed-JSONL
  injection, hang/timeout simulation, multi-turn conversations. These
  belong in parser unit tests with inline string literals, not in mock
  scripts.
- **Real `[judge]` criteria in v1** — the judge mechanism is documented
  in *Architecture / Judge Mechanism* for future use; loom-tests v1
  has no LLM-evaluated acceptance criteria. Every existing acceptance
  reduces to a deterministic check, so all current annotations are
  `[verify]`.
- **`cargo fuzz` under `nix flake check`** — exposed as `nix run .#fuzz-loom`
  for on-demand or nightly runs only. proptest covers invariants in CI.
- **Hard CI-time NFR** — the <5s `cargo nextest` budget on Linux is a
  soft design target, not a CI failure threshold. It guides decisions
  (no real sleeps, subprocess tests need justification, proptest case
  count bounded) but the test runner doesn't fail when the budget is
  exceeded; humans review timing in PRs.
