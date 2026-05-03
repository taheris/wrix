# Loom Tests

Test strategy and infrastructure for the Loom agent driver.

## Problem Statement

Loom is a Rust binary replacing Ralph's bash workflow scripts with a multi-crate
workspace. Testing spans two distinct domains: Rust-native unit/integration
tests (via `cargo test`) and system-level tests that exercise the full
host → container → agent pipeline. The Ralph test suite
([ralph-tests.md](ralph-tests.md)) validates bash script behavior with mock
Claude; Loom needs equivalent coverage in a Rust-native testing framework with SQLite
state database tests.

## Requirements

### Functional

1. **Three test tiers** with complementary scope:
   - **Unit tests** (`cargo test`) — per-crate, fast, no external dependencies.
     Run as part of `nix flake check`.
   - **Integration tests** (`cargo test --test`) — cross-crate, use mock agent
     processes, no containers. Run as part of `nix flake check`.
   - **System tests** (`nix run .#test-loom`) — full pipeline with `podman`,
     mock agent inside container, live `bd`. Separate runner because it needs
     a container runtime.

2. **Mock agent processes** — integration and system tests use mock executables
   that speak the agent protocols:
   - **Mock pi** — a script that reads NDJSON commands from stdin, emits
     canned NDJSON events to stdout, supports scenario-driven responses.
   - **Mock claude** — a script that emits canned stream-json NDJSON events
     to stdout and exits. Reuses the existing `tests/ralph/mock-claude`
     where possible.

3. **Unit test coverage by crate:**

   **loom-core:**
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
   - `bd` CLI wrapper uses `Command::arg()`, not shell interpolation (verify
     agent-crafted values with shell metacharacters are passed literally)
   - Config file loading (TOML parsing into `LoomConfig`), defaults when
     file is absent or fields are missing
   - `SpawnConfig` JSON serialization round-trips with stable field ordering
     and key names (the contract with `wrapix run-bead --spawn-config`).
     Adding a field is non-breaking; renaming or removing one is — the test
     pins the on-disk shape so changes surface as test failures, not silent
     wire-format drift

   **loom-agent:**
   - Pi RPC command serialization (Rust struct → NDJSON line)
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
   - Malformed NDJSON handling — specific test cases:
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
   - Timeout behavior: no NDJSON line for 5+ minutes → warning logged, no
     abort

   **loom-templates:**
   - All templates compile (caught at build time by Askama, but explicit test
     asserts no regressions)
   - Template rendering with known inputs produces expected output
   - Template output parity with Ralph's bash-rendered templates for identical
     variable values
   - Partial inclusion works (context pinning, exit signals, spec header, etc.)

   **loom-workflow:**
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
     `run_parallel_batch` (or its Rust equivalent) creates 3 worktrees,
     spawns 3 `wrapix run-bead` futures concurrently, and reports all
     results before merge-back
   - Parallel batch with N=1: no worktree is created; work runs on the
     driver branch (parity with current ralph behavior)
   - Merge-back ordering: branches merge to the driver branch sequentially,
     not in parallel (avoids index lock races)
   - On worker failure, the bead's worktree branch is deleted and the bead
     is queued for retry per the retry policy
   - On merge conflict, the worktree path is preserved and the bead is
     marked failed (does not silently overwrite or auto-resolve)

   **Concurrency & locking (loom-core):**
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

   **Auxiliary commands (loom-workflow):**
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
   - `loom logs` (no flags) prints the path of, and tails, the most recent
     file under `.wrapix/loom/logs/`
   - `loom logs --bead <id>` finds the most recent log for that bead;
     errors clearly if none exists
   - `loom spec --deps` parses the active spec's `[verify]` / `[judge]`
     annotations, opens each referenced test file, and prints the
     deduplicated set of nixpkgs needed (port of `ralph sync --deps`)
   - **CLI surface check**: `loom --help` lists exactly the v1 commands
     (workflow + auxiliary) and does NOT list `sync`, `tune`, or `watch`

   **Run UX renderer (loom-workflow):**
   - Default mode: header line per bead, one line per tool call, no
     streamed assistant text deltas
   - `--verbose` / `-v` streams assistant text as it arrives
   - Tool call rendering: tool name + truncated single-line summary; lines
     longer than terminal width are clipped, never wrapped
   - Status colors: green for `✓ done`, red for `✗ failed`, yellow for
     retry; disabled when stdout is not a TTY (`NO_COLOR` honored)
   - Parallel mode: tool-call lines are bead-id-prefixed so interleaved
     output stays attributable

   **Run logger (loom-workflow):**
   - Every bead spawn produces a file at
     `.wrapix/loom/logs/<spec-label>/<bead-id>-<timestamp>.ndjson` containing
     the raw event stream as one JSON object per line
   - File is written for every spawn, regardless of terminal verbosity
     (renderer and logger both subscribe to the same `AgentEvent` stream;
     verify by capturing both and asserting line-for-line equality on the
     log side)
   - With `--parallel N`, two concurrent spawns produce two distinct files;
     no shared writer, no interleaving
   - Log file path is logged at `info!` when the spawn starts so users can
     `tail -f` it
   - Retention sweep on `loom run` startup: with `retention_days = 14`,
     files mtime'd 15+ days ago are deleted, files mtime'd today are kept
   - `retention_days = 0` is a no-op (no files deleted, no walk)
   - Best-effort: a single un-deletable file (read-only, locked) does not
     abort the sweep — remaining deletable files are still removed

   **GitClient (loom-core):**
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
   - Hybrid implementation: tests verify that whichever path is used
     internally (gix vs `git` CLI) is encapsulated — callers see only
     the typed Rust API. No `gix::` or `tokio::process::Command::new("git")`
     references appear outside the `GitClient` module
     (judge-style structural assertion)

4. **Integration test coverage:**
   - Startup probe: mock pi responds to `get_commands`, verify driver
     proceeds; mock pi responds with missing required command, verify driver
     fails fast with version-mismatch error
   - `wrapix run-bead` invocation: loom writes a `SpawnConfig` JSON to a
     temp file, invokes a mock `wrapix-run-bead` (a script in `PATH` that
     reads the JSON, asserts required fields, then exec's a mock agent).
     Verifies the spawn-config contract end-to-end without needing a real
     container runtime
   - Spawn mock pi process, send prompt via stdin, receive events via stdout,
     verify `AgentEvent` stream
   - Spawn mock claude process, read stream-json events from stdout, verify
     `AgentEvent` stream
   - Full `run --once` flow with mock agent: bead selection → container
     spawn → prompt → events → bead close
   - Steering: send steer command to mock pi, verify it receives and
     responds correctly
   - Compaction: mock pi emits `compaction_start` event, verify driver sends
     re-pin content via steer, then receives `compaction_end`
   - Error recovery: mock agent exits mid-stream, verify remaining stdout
     is drained before reporting `ProcessExit`
   - Permission auto-approve: mock claude emits `control_request`, verify
     driver responds with `control_response` (approved: true) and logs tool
     name at `info!` level
   - Permission deny-list: configure `denied_tools`, verify `control_request`
     for a listed tool is rejected with `approved: false`
   - Parallel run end-to-end with mock agent: `loom run --parallel 2` with
     two ready beads spawns two mock-agent processes concurrently (verified
     via overlapping spawn timestamps captured by the mock), each in its
     own temp git worktree, then merges both branches back

5. **System test coverage:**
   - `loom plan -n <label>` with mock agent in container creates spec file
   - `loom todo` with mock agent creates beads molecule
   - `loom run --once` with mock agent completes single bead
   - `loom run` continuous mode completes all beads in molecule
   - `loom check` push gate exercises git push / beads-push mocks
   - `loom msg` lists and resolves clarify beads
   - Profile selection: bead with `profile:rust` spawns rust container image
   - Runtime composition: `profile:rust` + `WRAPIX_AGENT=pi` produces image
     with both rust toolchain and pi binary
   - Agent switching: same test with `WRAPIX_AGENT=pi` and
     `WRAPIX_AGENT=claude` produces equivalent end state — same beads
     closed, same files written, same final commit on the bead branch
     (modulo agent-specific cost/timing fields)

6. **State database tests:**
   - `StateDb::open` on nonexistent path creates DB with correct schema
   - `StateDb::open` on existing DB is idempotent (no data loss)
   - `StateDb::rebuild` globs `specs/*.md` and calls `bd list` / `bd mol
     progress` to populate specs and molecules tables
   - Rebuild resets `iteration_count` to 0 for all molecules
   - Corrupted DB file (truncated, zero-length) → `loom init --rebuild`
     recovers without error
   - `meta` table stores `current_spec` and `schema_version`

7. **Rust style enforcement:**
   - No `unwrap()`, `todo!()`, `panic!()`, `unimplemented!()` in non-test code
     (grep-based check in CI)
   - No `#[allow(dead_code)]` in non-test code
   - No `derive(From)` or `derive(Into)` on newtype structs
   - `cargo clippy` passes with workspace lints
   - No `types.rs` or `error.rs` at crate roots

### Non-Functional

1. **Deterministic** — no real LLM API calls. Mock agents return canned
   responses.
2. **Fast** — `cargo test` (unit + integration) completes in <30s. System
   tests complete in <120s.
3. **Isolated** — each test uses its own temp directory and beads database
   prefix. No shared mutable state between tests.
4. **Parallel-safe** — unit and integration tests run in parallel via
   `cargo test`'s default threading. System tests use per-test beads isolation
   (same pattern as ralph-tests.md).
5. **CI-friendly** — `nix flake check` runs unit + integration tests.
   System tests available via separate `nix run .#test-loom` entry point.
6. **Real bd** — system tests run against live `bd` (not a mock), consistent
   with ralph-tests.md's approach.

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
        logging.rs            # Integration: tee'd renderer + log sink
    loom-agent/
      src/
        pi/
          mod.rs
          protocol.rs         # NDJSON serialization/deserialization + inline tests
          fixtures/           # captured pi NDJSON sessions
        claude/
          mod.rs
          protocol.rs         # stream-json NDJSON event parsing + inline tests
          fixtures/           # captured claude stream-json sessions
        ...
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
        render.rs             # Integration: full-template rendering parity
    loom/
      tests/
        run_smoke.rs          # Integration: CLI smoke tests against the binary

tests/
  loom-test.sh                # Top-level orchestrator (cargo test + system tests)
  loom/
    default.nix               # Nix test derivation
    run-tests.sh              # System test harness
    mock-pi                   # Mock pi executable (NDJSON RPC)
    mock-claude               # Mock claude executable (or symlink to ralph's)
    lib/
      assertions.sh           # Shared assertion helpers
      fixtures.sh             # Test setup/teardown
    scenarios/
      happy-path.sh           # Full workflow scenario
      steering.sh             # Mid-session steering scenario
      compaction.sh           # Compaction + re-pin scenario
      error-recovery.sh       # Agent failure scenarios
```

### Parse, Don't Validate: Boundary Test Patterns

Each boundary layer (see loom-harness.md) has dedicated tests that verify the
parse-once-use-everywhere contract:

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

#[test]
fn state_db_stores_and_retrieves_spec() {
    let dir = tempdir().unwrap();
    let db = StateDb::open(&dir.path().join("state.db")).unwrap();
    db.set_current_spec(&SpecLabel::new("auth")).unwrap();
    let label = db.current_spec().unwrap();
    assert_eq!(label.unwrap().as_str(), "auth");
}
```

### Protocol Fixtures

Captured from actual agent output. Each fixture is a multi-line NDJSON file
representing one complete session. Fixtures live alongside the parser code in
loom-agent's module directories.

```
loom/crates/loom-agent/src/
  claude/
    fixtures/
      happy-path.ndjson          # system → assistant → tool_use → tool_result → result
      error-session.ndjson       # system → result (is_error: true)
      multi-turn.ndjson          # system → assistant → result (multiple turns)
      permission-request.ndjson  # system → assistant → control_request → assistant → result
      unknown-events.ndjson      # includes event types not in our enum
  pi/
    fixtures/
      happy-path.ndjson          # prompt → message_update(text_delta) → tool_execution_start → tool_execution_end → turn_end → agent_end
      compaction.ndjson          # prompt → compaction_start → steer → compaction_end → agent_end
      extension-ui.ndjson        # prompt → extension_ui_request → message_update(text_delta) → turn_end → agent_end
```

### Mock Pi Design

Mock pi speaks the same NDJSON RPC protocol as real pi:

```bash
#!/usr/bin/env bash
set -euo pipefail

SCENARIO="${MOCK_PI_SCENARIO:-happy-path}"

while IFS= read -r line; do
  type="$(echo "$line" | jq -r '.type')"
  case "$type" in
    prompt)
      # Emit canned events for this scenario phase
      emit_events "$SCENARIO" "prompt"
      ;;
    steer)
      # Acknowledge steering
      echo '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","text":"Adjusting approach..."}}'
      ;;
    abort)
      echo '{"type":"agent_end","messages":[]}'
      exit 0
      ;;
  esac
done
```

Scenarios define the event sequence for each command type. The mock supports:
- Multi-turn conversations (prompt → events → follow_up → events)
- Tool call simulation (emit tool_execution_start, wait for implicit execution,
  emit tool_execution_end)
- Compaction events (emit compaction_start, expect steer with re-pin content,
  emit compaction_end)
- Error injection (malformed NDJSON, early exit, timeout)
- Hang simulation (sleep before responding, to test read timeout warning)

### State Database Test Pattern

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

### Template Parity Test Pattern

```rust
#[test]
fn run_template_parity() {
    // Render with Askama
    let ctx = RunTemplate {
        label: SpecLabel::new("auth"),
        spec_path: "specs/auth.md".into(),
        issue_id: Some(BeadId::new("wx-123").unwrap()),
        title: Some("Implement parser".into()),
        // ...
    };
    let loom_output = ctx.render().unwrap();

    // Compare against bash-rendered reference (captured fixture)
    let bash_output = include_str!("fixtures/bash-rendered-run.md");

    // Normalize whitespace, compare
    assert_eq!(normalize(loom_output), normalize(bash_output));
}
```

### Nix Integration

```nix
# tests/loom/default.nix
{ loom, bd, ... }:
{
  # Unit + integration tests (part of nix flake check)
  loom-tests = runCommandLocal "loom-tests" {
    nativeBuildInputs = [ loom.cargoDeps rustToolchain ];
  } ''
    cd ${loom.src}
    cargo test --workspace
    mkdir -p $out
  '';

  # System tests (separate runner)
  loom-integration = writeShellApplication {
    name = "test-loom";
    runtimeInputs = [ loom bd podman jq ];
    text = builtins.readFile ./run-tests.sh;
  };
}
```

## Affected Files

### New

| File | Role |
|------|------|
| `tests/loom-test.sh` | Top-level test orchestrator ([verify] tag target) |
| `loom/crates/*/src/**/*.rs` | Source files carry inline `#[cfg(test)] mod tests` blocks |
| `loom/crates/*/tests/*.rs` | Per-crate cargo integration tests |
| `loom/crates/loom-agent/src/*/fixtures/` | Protocol NDJSON fixtures (pi + claude) |
| `loom/crates/loom-templates/src/fixtures/` | Bash-rendered template fixtures |
| `tests/loom/default.nix` | Nix test derivation |
| `tests/loom/run-tests.sh` | System test harness |
| `tests/loom/mock-pi` | Mock pi executable |
| `tests/loom/mock-claude` | Mock claude executable (or symlink) |
| `tests/loom/lib/` | Test library modules |
| `tests/loom/scenarios/` | Test scenario definitions |

### Modified

| File | Change |
|------|--------|
| `modules/flake/tests.nix` | Add loom unit tests to `nix flake check` |
| `flake.nix` | Expose `test-loom` app |

## Success Criteria

### Unit tests

- [ ] `cargo test -p loom-core` passes
  [verify](tests/loom-test.sh::test_cargo_test_core)
- [ ] `cargo test -p loom-agent` passes
  [verify](tests/loom-test.sh::test_cargo_test_agent)
- [ ] `cargo test -p loom-workflow` passes
  [verify](tests/loom-test.sh::test_cargo_test_workflow)
- [ ] `cargo test -p loom-templates` passes
  [verify](tests/loom-test.sh::test_cargo_test_templates)
- [ ] Newtype serde round-trip tests cover all ID types
  [verify](tests/loom-test.sh::test_newtype_serde_roundtrip)
- [ ] State database round-trip tests cover spec, molecule, and meta operations
  [verify](tests/loom-test.sh::test_state_db_roundtrip)
- [ ] Pi RPC protocol tests cover all command/event types
  [verify](tests/loom-test.sh::test_pi_protocol_coverage)
- [ ] Claude stream-json protocol tests cover all event types
  [verify](tests/loom-test.sh::test_claude_protocol_coverage)
- [ ] Template rendering tests produce expected output for all templates
  [verify](tests/loom-test.sh::test_template_rendering)

### State database

- [ ] `StateDb::open` creates schema on first open
  [verify](tests/loom-test.sh::test_state_db_init)
- [ ] `StateDb::open` is idempotent on existing DB
  [verify](tests/loom-test.sh::test_state_db_idempotent)
- [ ] `StateDb::rebuild` populates from spec files and active beads
  [verify](tests/loom-test.sh::test_state_db_rebuild)
- [ ] Rebuild resets iteration counters to 0
  [verify](tests/loom-test.sh::test_state_db_rebuild_resets_counters)
- [ ] Corrupted DB → `loom init --rebuild` recovers
  [verify](tests/loom-test.sh::test_state_corruption_recovery)
- [ ] `current_spec` round-trips via meta table
  [verify](tests/loom-test.sh::test_state_current_spec)

### Template parity

- [ ] Every Askama template renders identically to bash for same inputs
  [verify](tests/loom-test.sh::test_template_output_parity)
- [ ] Bash-rendered fixtures checked into repo as compatibility contracts
  [verify](tests/loom-test.sh::test_template_fixtures_exist)

### Integration tests

- [ ] Mock pi process spawns and speaks NDJSON RPC correctly
  [verify](tests/loom-test.sh::test_mock_pi_rpc)
- [ ] Mock claude process spawns and emits stream-json NDJSON events correctly
  [verify](tests/loom-test.sh::test_mock_claude_stream_json)
- [ ] Steering flow: prompt → steer → updated response
  [verify](tests/loom-test.sh::test_steering_flow)
- [ ] Compaction flow: compaction event → re-pin via steer
  [verify](tests/loom-test.sh::test_compaction_repin_flow)
- [ ] Error recovery: agent exits mid-stream → graceful handling
  [verify](tests/loom-test.sh::test_agent_midstream_exit)
- [ ] Malformed NDJSON: truncated JSON, wrong shape, empty lines → correct handling
  [verify](tests/loom-test.sh::test_malformed_ndjson_handling)
- [ ] Permission auto-approve: control_request → control_response with approved:true
  [verify](tests/loom-test.sh::test_permission_autoapprove_flow)
- [ ] Read timeout: no data for 5 min → warning logged, session continues
  [verify](tests/loom-test.sh::test_read_timeout_warning)
- [ ] Parallel run end-to-end: `--parallel 2` with two ready beads dispatches
      two mock-agent spawns concurrently and merges both branches back
  [verify](tests/loom-test.sh::test_parallel_run_end_to_end)
- [ ] `GitClient` API: create worktree, list, remove, status, merge round-trip
      against a temp repo
  [verify](tests/loom-test.sh::test_git_client_roundtrip)
- [ ] `GitClient` is the only module importing `gix` or invoking the `git` CLI
  [judge](tests/judges/loom.sh::test_git_client_encapsulation)

### System tests

- [ ] `loom plan -n <label>` with mock agent creates spec file
  [verify](tests/loom-test.sh::test_system_plan)
- [ ] `loom todo` with mock agent creates beads molecule
  [verify](tests/loom-test.sh::test_system_todo)
- [ ] `loom run --once` with mock agent completes single bead
  [verify](tests/loom-test.sh::test_system_run_once)
- [ ] `loom run` continuous mode completes all beads
  [verify](tests/loom-test.sh::test_system_run_continuous)
- [ ] `loom check` exercises push gate with git/beads mocks
  [verify](tests/loom-test.sh::test_system_check_push_gate)
- [ ] `loom msg` lists and resolves clarify beads
  [verify](tests/loom-test.sh::test_system_msg)
- [ ] Profile selection: `profile:rust` bead spawns rust container
  [verify](tests/loom-test.sh::test_system_profile_selection)
- [ ] Runtime composition: `profile:rust` + `WRAPIX_AGENT=pi` has both rust and pi
  [verify](tests/loom-test.sh::test_system_runtime_composition)
- [ ] Agent switching: `WRAPIX_AGENT=pi` and `claude` produce the same
      beads closed, same files written, same final commit on the bead
      branch (modulo agent-specific cost/timing fields)
  [verify](tests/loom-test.sh::test_system_agent_switching)
- [ ] Full workflow: plan → todo → run → check end-to-end
  [verify](tests/loom-test.sh::test_system_full_workflow)

### Style enforcement

- [ ] No `unwrap()`/`todo!()`/`panic!()`/`unimplemented!()` in non-test code
  [verify](tests/loom-test.sh::test_no_panics_in_production)
- [ ] No `#[allow(dead_code)]` in non-test code
  [verify](tests/loom-test.sh::test_no_allow_dead_code)
- [ ] No `derive(From)` or `derive(Into)` on newtype structs
  [verify](tests/loom-test.sh::test_no_derive_from_on_newtypes)
- [ ] `cargo clippy` passes with workspace lints
  [verify](tests/loom-test.sh::test_clippy_clean)
- [ ] No `types.rs` or `error.rs` at crate roots
  [verify](tests/loom-test.sh::test_no_central_type_files)

### CI integration

- [ ] `nix flake check` includes loom unit + integration tests
  [verify](tests/loom-test.sh::test_flake_check_includes_loom)
- [ ] `nix run .#test-loom` runs system tests
  [verify](tests/loom-test.sh::test_system_runner_exists)
- [ ] `cargo test` completes in <30s
  [verify](tests/loom-test.sh::test_cargo_test_timing)
- [ ] System tests complete in <120s
  [verify](tests/loom-test.sh::test_system_test_timing)

## Out of Scope

- **Real LLM API tests** — no actual API calls in any tier.
- **Performance benchmarks** — timing targets are for the test suite itself,
  not for Loom's runtime performance.
- **Ralph bash test migration** — ralph-tests.md tests remain as-is. They
  validate bash behavior; Loom tests validate Rust behavior. Both run in CI.
- **End-to-end tests with real pi-mono** — system tests use mock pi. Testing
  against real pi-mono is manual validation during development.
- **macOS system tests** — system tests require `podman` (Linux). Darwin
  container testing is a follow-up.
- **Mocking `bd`** — system tests use live `bd`, consistent with
  ralph-tests.md (see NFR #6).
- **System-tier parallel-run coverage** — `loom run --parallel N` is fully
  exercised at the integration tier (worktree creation, concurrent spawns,
  merge-back). Repeating with real containers adds podman time without
  catching new failure modes.

## Implementation Notes

### Reusing Ralph's mock infrastructure

Ralph's `tests/ralph/mock-claude` can be symlinked or adapted for Loom's mock
claude. The scenario format is the same (shell or JSON). Loom's mock-pi is new
but follows the same pattern.

### Fixture capture workflow

To capture bash-rendered template fixtures:
1. Run `ralph run --once` with a known bead in a test environment
2. Intercept the rendered prompt (from the `--append-system-prompt` argument)
3. Save as `fixtures/bash-rendered-run.md`
4. Check into the repo

Update fixtures when Ralph's bash templates change.

### Test isolation for bd

System tests use the same isolation pattern as ralph-tests.md: snapshot-based
`init_beads` that copies a pre-seeded `.beads/` directory per test. Each test
gets its own embedded Dolt database, fully isolated from peers.
