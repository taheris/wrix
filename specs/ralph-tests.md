# Ralph Integration Tests

Integration tests for the ralph workflow with unified test infrastructure.

## Problem Statement

The ralph workflow orchestrates AI-driven feature development. Manual testing is
slow and doesn't catch regressions, and ralph-specific coverage needs a runtime
environment (mock claude, live `bd`) that doesn't fit inside `nix flake check`.
This spec defines the integration suite that fills that gap.

## Requirements

### Functional

1. **Test entry points** — two runners with complementary scope:
   - `nix run .#test` → `nix flake check`: fast pure checks (smoke, unit, darwin
     logic, ralph utility tests, template validation). Runs on all platforms; VM
     integration tests skip on non-Linux / no-KVM.
   - `nix run .#test-ralph` → ralph integration suite with mock claude and live
     `bd` (the subject of this spec). Separate because it needs a runtime
     environment and is slower than `nix flake check`.

2. **Mock Claude interface** — Tests use a mock `claude` executable that:
   - Receives prompts via command-line arguments
   - Reads scenario files to determine responses
   - Executes side effects (creates files, runs `bd` commands)
   - Outputs responses with appropriate exit signals

3. **Scenario-driven tests** — Each test case defines:
   - Initial state (label, spec content if applicable)
   - Mock responses for each phase
   - Expected end state (files, beads issues, exit codes)

4. **Full workflow coverage** — Tests verify:
   - `ralph plan <label>` creates spec file with `RALPH_COMPLETE` signal
   - `ralph todo` creates beads issues with correct dependencies
   - `ralph run --once` works issues in dependency order
   - `ralph run` processes all issues until complete

5. **Parallel agent simulation** — Tests verify coordination:
   - `run --once` marks issue `in_progress` before starting work
   - Subsequent `run --once` (simulated) skips in_progress items
   - Subsequent `run --once` skips items blocked by in_progress dependencies

6. **Error handling** — Tests verify:
   - Missing exit signal (no `RALPH_COMPLETE`) — run does not close issue
   - `RALPH_BLOCKED: reason` signal — workflow pauses appropriately
   - Invalid beads JSON output — graceful handling
   - Partial completion — epic remains open when tasks remain

7. **Single-command planning** — `ralph plan <label>` performs both setup and the
   interview in one invocation. Setup steps are idempotent (safe to rerun). No
   separate `ralph start` command exists.

8. **Implementation Notes section** — Support transient context in specs:
   - Specs may contain `## Implementation Notes` section
   - This section is available during `ralph todo` for context when creating beads
   - Section is stripped when spec is finalized to `specs/<feature>.md`
   - Useful for capturing bugs, gotchas, and implementation hints that don't belong in permanent docs

9. **Extended command coverage** — Tests exercise every ralph subcommand:
   - `status` — current mol position, awaiting-input display, `--spec`/`-s`/`--all` flags, missing state
   - `logs` — error detection, `--all`, context lines, `--spec` resolution, missing logfile, exit-code errors
   - `sync` — fresh install, backup, dry-run, partials; `--diff` template/partial filtering, pipe-friendly output
   - `check` — template/partial validation, push gate, iteration counter, auto-iterate via run, clarify-stops-push, dolt pull/push
   - `use` — switch active workflow, hidden specs, missing spec/state errors, no-label error, plain-text output
   - `msg` — reply resume hint
   - `spec` — verify/judge/all annotations, filter single, short flags, grouped output, summary line, nonzero exit, empty skip
   - `tune` — help, mode detection (interactive vs integration), env validation, prompt building

10. **Concurrent workflow coverage** — Tests verify per-label state isolation:
    - Per-label `state.json` files under `.wrapix/ralph/state/`
    - `--spec <label>` and `-s <label>` flags on `todo`/`run`/`status`/`logs`
    - `--current` pointer semantics (read-once, mid-switch safety, plain-text format)
    - Cross-spec isolation — multiple `ralph plan` calls produce independent state
    - Pre-per-label serial workflows (no `current` pointer) still run unchanged

11. **Spec-change detection** — `ralph todo` uses git diff to find changed requirements:
    - `base_commit` tracked in state.json, advanced on success (and on post-sync warning)
    - `--since <commit>` override; orphaned-commit fallback; molecule fallback; new-mode fallback
    - README discovery path for reconstructed state when molecule missing
    - Uncommitted-changes error; `implementation_notes` cleared on success
    - No `base_commit` written on failure or for hidden specs

12. **Beads sync (Dolt) coverage** — Tests verify bidirectional sync:
    - Host-side `bd dolt pull` after `ralph todo` / `ralph run` complete
    - Container-side `bd dolt push` for `plan` / `todo` / `run` (once and continuous) / `check`
    - Failure modes: container commit failure, push failure, host sync failure, post-sync warning recovery
    - `ralph run` does not push (only reads); startup `bd dolt pull` behavior
    - Todo-created beads visible to subsequent `ralph run`

13. **Template infrastructure coverage** — Tests verify the template system:
    - Companion templates (variable injection, partial override, wiring)
    - Re-pin hooks (settings shape, script output, content size, host-commands exclusion)
    - Runtime dir (gitignored, env propagation, cleanup on exit)
    - Entrypoint settings merge (ralph settings concatenated with host settings/hooks)
    - Manifest reading (empty, format, missing directory/manifest)
    - Molecule discovery from README

14. **Amortized test setup** — Per-test overhead is bounded so the suite meets NFR
    §2's <90s target. See *Performance Infrastructure* below for the concrete
    mechanisms (snapshot DB, shared Dolt server, batched fixtures, metadata cache,
    tuned parallelism).

15. **Compaction re-pin simulation** — Tests verify the `SessionStart[compact]`
    re-pin hook end-to-end, *excluding* Claude's own hook dispatcher:
    - A test helper (`simulate_compact_event`) reads `claude-settings.json`, locates
      the `SessionStart` entry with `matcher: "compact"`, executes its hook script,
      and captures `hookSpecificOutput.additionalContext`.
    - Tests invoke the helper after each ralph command (`plan`, `todo`, `run --once`,
      `check`) and assert the returned orientation reflects **current** state
      (claimed issue id, current molecule, active label) — not stale startup state.
    - Explicitly validates refresh-per-invocation: `repin.content` must be rewritten
      whenever ralph state changes that the orientation references.
    - Out of scope: exercising Claude's own dispatch of the hook after a real
      compaction. That remains Claude's contract; the helper simulates the dispatch
      boundary.

### Non-Functional

1. **Deterministic** — Tests produce consistent results (no real Claude API calls)
2. **Fast** — Full suite completes in <90s on dev hardware. See *Performance Infrastructure* for how this is achieved.
3. **Isolated** — Each test runs in a clean temp directory with its own beads database prefix
4. **Skip-aware** — Darwin tests skip gracefully on Linux
5. **Clean beads** — No test beads persist after run; temp dirs removed in teardown
6. **Real bd** — Tests run against live `bd` (not a mock) so semantic behaviors (dep ordering, status filtering, label selectors, JSON shape) are verified end-to-end

## Affected Files

| File | Change |
|------|--------|
| `tests/ralph/lib/fixtures.sh` | Snapshot-based `init_beads`; batched setup; `simulate_compact_event` helper; `setup_host_mocks` for live-path tests |
| `tests/ralph/lib/runner.sh` | Tuned default `RALPH_TEST_MAX_JOBS` |
| `tests/lib/dolt-server.sh` | Shared Dolt SQL server lifecycle |
| `lib/ralph/cmd/{check,run,todo,plan,watch,msg}.sh` | Host-side guard parameterized via `WRAPIX_CLAUDE_CONFIG` to enable live-path tests |

### Test Directory Structure

```
tests/ralph/
├── default.nix              # Nix test derivation
├── mock-claude              # Mock claude executable
├── run-tests.sh             # Test harness (thin wrapper)
├── templates.nix            # Template test fixtures
├── lib/                     # Reusable test libraries
│   ├── assertions.sh        # test_pass, test_fail, assert_*
│   ├── fixtures.sh          # setup_*, teardown_*, simulate_compact_event
│   ├── mock-claude.sh       # Mock infrastructure
│   └── runner.sh            # Parallel/sequential test execution
└── scenarios/               # Test scenario definitions (shell + JSON formats)
```

Scenario files cover the signals and edge cases exercised by the suite (happy
path, `RALPH_BLOCKED`, `RALPH_CLARIFY`, missing signal, malformed output, etc.).
See `tests/ralph/scenarios/` for the current set rather than enumerating here —
the list evolves with coverage.

### Darwin Test Reorganization

Darwin tests live under `tests/darwin/` (migrated from the previous
`tests/darwin-*` flat layout).

## Test Exit Code Convention

Standalone shell test scripts use special exit codes to distinguish pass, fail, skip, and not-yet-implemented results. The test runner treats skip and not-implemented as non-failures.

### Exit Codes

| Exit Code | Meaning | When to Use |
|-----------|---------|-------------|
| 0 | Pass | Test ran and succeeded |
| 1 | Fail | Test ran and failed |
| 77 | Skip | Test cannot run on this platform/environment (legitimate) |
| 78 | Not Yet Implemented | Test exists for a feature that hasn't been built yet |

Exit code 77 follows the convention used by Automake, TAP, and GNU test frameworks. Exit code 78 is project-specific (unused by convention and available for custom meaning).

### When to Use Exit 77 (Skip)

Use exit 77 when a test cannot run due to platform or environment constraints that are outside the test's control:

- **Platform checks** — Darwin-only test running on Linux, or vice versa
- **Hardware requirements** — KVM availability, GPU, specific CPU features
- **Runtime conditions** — Container system not running, notification daemon not available
- **Upstream limitations** — `bd` features with known behavioral constraints (e.g., blocked-by-in_progress filtering)

Use `test_skip` from `assertions.sh` to exit with code 77 and print a message:

```bash
[[ "$(uname)" == "Darwin" ]] || test_skip "Requires macOS"
```

### When to Use Exit 78 (Not Yet Implemented)

Use exit 78 when a test exists for a feature that genuinely hasn't been built yet:

- **Config option doesn't exist** — e.g., `loop.max-iterations` is spec'd but not implemented
- **Feature not built** — the test is a placeholder for planned functionality
- **API not available** — the function or command the test exercises doesn't exist yet

Use `test_not_implemented` from `assertions.sh` to exit with code 78 and print a message:

```bash
test_not_implemented "loop.max-iterations config option not yet implemented"
```

### How the Test Runner Handles These Codes

The test runner (`runner.sh`) executes each test in an isolated subshell via `run_test_isolated`. An EXIT trap captures the exit code and categorizes it:

- Exit 77 increments the **skipped** counter
- Exit 78 increments the **not_implemented** counter
- Neither counts as a failure — the overall test suite passes as long as the **failed** counter is zero

### Summary Format

The test runner prints results in this format:

```
Results: 45 passed, 0 failed, 3 skipped (exit 77), 4 not implemented (exit 78)
```

CI systems can monitor skip and not-implemented counts to detect unexpected changes (e.g., a skip count increasing may indicate a regression in test infrastructure).

### Nix Derivation Tests

Nix derivation tests (`*.nix`) use `runCommandLocal`, which treats any non-zero exit code as a build failure. To use exit 77 for skips while keeping the Nix derivation successful, add an EXIT trap at the top of the script that catches exit 77, creates `$out`, and converts the exit to 0:

```bash
runCommandLocal "test-name" { } ''
  # Nix-safe exit 77 handler: treat skip (77) as build success
  trap '_ec=$?; if [ "$_ec" -eq 77 ]; then mkdir -p $out; exit 0; fi' EXIT

  set -euo pipefail

  if [ "$(uname)" != "Darwin" ]; then
    echo "SKIP: Darwin-only test" >&2
    exit 77
  fi

  # ... test logic ...
  mkdir -p $out
''
```

The trap must appear before `set -euo pipefail` so it is registered before any exit occurs. Skip messages go to stderr (`>&2`) so they remain visible in build logs even when the derivation succeeds.

### Skip Messages (NFR1)

Every skip must print a message explaining why the test was skipped. The message should state what prerequisite is missing and, where possible, how to provide it. Both `test_skip` and `test_not_implemented` print to stderr automatically.

## Test Library Modules

The test infrastructure is split into reusable libraries under `tests/ralph/lib/`:

### `assertions.sh`

Provides assertion functions for test validation:
- `test_pass <name>` — Record test success
- `test_fail <name> <reason>` — Record test failure
- `assert_file_exists <path>` — Verify file presence
- `assert_file_contains <path> <pattern>` — Grep file for content
- `assert_exit_code <expected> <actual>` — Compare exit codes
- `assert_beads_count <n>` — Verify number of beads created

### `fixtures.sh`

Test setup and teardown helpers:
- `setup_test_env` — Create isolated temp directory; batched `mkdir -p` and bulk symlinks
- `teardown_test_env` — Clean up temp directory
- `setup_ralph_config` — Initialize `.wrapix/ralph/config.nix`
- `create_test_spec <label> <content>` — Create spec file for testing
- `init_beads` — Populate `.beads/` from the pre-seeded snapshot and register the test's unique prefix on the shared Dolt server (no per-test `bd init`)
- `simulate_compact_event` — Read `claude-settings.json`, locate the `SessionStart[compact]` hook, execute it, and return `hookSpecificOutput.additionalContext`. Used to verify re-pin content reflects current ralph state without needing real Claude compaction.
- `setup_host_mocks [--no-git-init]` — Overlay mocks for `wrapix`, `git push`, `beads-push`, and the `ralph` dispatcher so ralph check/run host-side branches run end-to-end without a real container. Forces the host guard via `WRAPIX_CLAUDE_CONFIG=/nonexistent`. Records invocations under `$MOCK_LOG_DIR/<name>.log`. Tunable per-test via `MOCK_WRAPIX_EXIT`, `MOCK_WRAPIX_HOOK` (inject fix-up beads), `MOCK_GIT_PUSH_EXIT`, `MOCK_BEADS_PUSH_EXIT`, and `MOCK_RALPH_DISPATCH` (which sub-commands to forward to `ralph-<cmd>` vs record-only).
- `assert_mock_called <name> <pattern>` / `assert_mock_not_called <name>` — Assert that a host-side mock was / was not invoked.

### Live-Path Testing (host-side branch)

Several ralph commands (`plan`, `todo`, `run`, `check`, `watch`, `msg --chat`) split
execution between a host-side branch (pre/post container) and a container-side
branch. The host-side branch is reached only when the process is NOT inside a wrapix
container, which is detected via `/etc/wrapix/claude-config.json`. Integration tests
run INSIDE a wrapix container, so the host guard in every split script is
parameterized:

```bash
if [ ! -f "${WRAPIX_CLAUDE_CONFIG:-/etc/wrapix/claude-config.json}" ] && command -v wrapix &>/dev/null; then
  # host-side branch …
```

`setup_host_mocks` sets `WRAPIX_CLAUDE_CONFIG=/nonexistent` and places mocks for
`wrapix`, `git push`, `beads-push`, and the `ralph` dispatcher on `PATH`, forcing
the host-side branch and recording invocations. Tests then assert behavior
end-to-end (verdict, `do_push_gate`, clarify detection, iteration counter,
`run → check` handoff) rather than grepping source for patterns. Production
behavior is unchanged (the env var is unset, the default path exists in the real
container image).

### `mock-claude.sh`

Mock Claude infrastructure:
- `setup_mock_claude` — Install mock executable in PATH
- `load_scenario <name>` — Load scenario file (shell or JSON)
- `get_phase_response <phase>` — Return response for current phase
- `execute_phase_effects <phase>` — Run side effects (bd commands, file creation)

### `runner.sh`

Test execution framework:
- `run_test_isolated <func> <result> <output>` — Run test in subshell
- `run_tests_parallel <tests...>` — Execute tests concurrently, capped by `RALPH_TEST_MAX_JOBS`
- `run_tests_sequential <tests...>` — Execute tests in order (for tests that share state)
- `summarize_results` — Print pass/fail/skip/not-implemented counts

## Performance Infrastructure

The ralph test suite runs against **live `bd`** (not a mock) so semantic drift is caught
end-to-end. To keep this fast, per-test overhead is amortized:

- **Shared Dolt server** — `tests/lib/dolt-server.sh` starts a single `dolt sql-server`
  per test run, used to create the initial snapshot. Orphaned servers from prior runs
  are reaped at startup.
- **Snapshot-based DB init** — `bd init` runs once to produce a template `.beads/`
  directory with an embedded Dolt DB. `init_beads` `cp -a`'s this template per test
  instead of re-running `bd init`; each test gets its own embedded Dolt, fully
  isolated from peers.
- **All-parallel execution** — tests run concurrently via `run_tests_parallel`.
  Per-test embedded Dolt means no shared MySQL server is on the write path, so there
  is no concurrent-write ceiling beyond host CPU/memory. An earlier sequential tier
  (for shared-server load avoidance) was removed once embedded Dolt landed.
- **Batched fixture ops** — `setup_test_env` does a single `mkdir -p` with all paths
  and symlinks all ralph commands in one loop. Avoids per-test fork/exec storms.
- **Template metadata cache** — `nix eval` runs once per run to produce
  `variables.json` / `templates.json`. Shared across tests via `RALPH_METADATA_DIR`
  (pre-set by the Nix test wrapper; generated on first use otherwise).
- **Tunable parallelism** — `RALPH_TEST_MAX_JOBS` caps concurrent tests. Default 16
  on host (balances CPU/memory); inside the wrapix container it is clamped to 5 to
  avoid overwhelming Claude.

## Mock Claude Design

### Mock Executable

```bash
#!/usr/bin/env bash
# mock-claude - receives prompt, returns scenario-defined response

SCENARIO_FILE="${MOCK_SCENARIO:-}"
PROMPT="$*"

# Read scenario, match phase, execute side effects, output response
```

### Scenario File Formats

Scenarios can be defined in shell (`.sh`) or JSON (`.json`) format.

**Shell format** (imperative, full control):
```bash
# scenarios/happy-path.sh

phase_plan() {
  # Create spec file
  cat > "$SPEC_PATH" << 'EOF'
# Test Feature
...
EOF
  echo "RALPH_COMPLETE"
}

phase_todo() {
  # Create beads issues
  bd create --title="Task 1" --type=task --labels="spec:$LABEL"
  bd create --title="Task 2" --type=task --labels="spec:$LABEL"
  bd dep add beads-002 beads-001
  echo "RALPH_COMPLETE"
}

phase_run() {
  # Implement and signal completion
  echo "Implemented the feature"
  echo "RALPH_COMPLETE"
}
```

**JSON format** (declarative, simpler):
```json
{
  "name": "happy-path",
  "description": "Full workflow from plan to completion",
  "phases": {
    "plan": {
      "output": "I'll create a spec for this feature...",
      "signal": "RALPH_COMPLETE",
      "creates_spec": true
    },
    "todo": {
      "output": "Creating tasks from the spec...",
      "signal": "RALPH_COMPLETE",
      "tasks": [
        {"title": "Task 1", "type": "task"},
        {"title": "Task 2", "type": "task", "depends_on": ["Task 1"]}
      ]
    },
    "run": {
      "output": "Implemented the feature",
      "signal": "RALPH_COMPLETE"
    }
  }
}
```

JSON scenarios are converted to shell phases by the test runner.

### Phase Detection

Mock determines current phase from:
- Prompt content patterns (e.g., "spec interview" → plan)
- Environment variables set by ralph
- Scenario file state

## Test Cases

### Happy Path

1. `ralph plan test-feature` — creates spec, signals complete
2. `ralph todo` — creates epic + tasks with dependencies
3. `ralph run --once` — completes first unblocked task
4. `ralph run` — completes remaining tasks, closes epic

### Parallel Simulation

1. Run `ralph run --once` — marks task A as `in_progress`
2. Simulate second agent state check — task B (no deps) available, task C (depends on A) blocked
3. Verify task selection logic

### Config Behavior

1. **spec.hidden = true** — spec file created in `state/` instead of `specs/`, README not updated
2. **spec.hidden = false** — spec file created in `specs/`, README updated with WIP entry
3. **beads.priority** — issues created with configured priority (test with priority=1 vs priority=3)
4. **loop.max-iterations** — run stops after N iterations even if work remains
5. **loop.pause-on-failure = true** — run pauses when iteration fails
6. **loop.pause-on-failure = false** — run continues after iteration failure
7. **loop.pre-hook / loop.post-hook** — hooks execute before/after each iteration
8. **failure-patterns** — custom patterns trigger configured actions (log/pause)

### Error Scenarios

1. **No completion signal** — `run --once` runs, mock omits `RALPH_COMPLETE`, verify issue stays open
2. **RALPH_BLOCKED signal** — mock returns `RALPH_BLOCKED: needs API key`, verify workflow pauses
3. **Malformed bd output** — `bd list` returns warning + JSON, verify parsing succeeds
4. **Partial epic** — close 2 of 3 tasks, verify epic stays open

### Extended Commands

Each ralph subcommand beyond `plan`/`todo`/`run` has direct coverage (see
Functional §9 for the per-command assertion list). Representative behaviors:
`status` mol-position rendering, `logs` error detection with context lines,
`sync --diff` markdown output, `check` push-gate and iteration counter, `use`
active-workflow switching, and `spec` verify/judge annotation parsing.

### Host-side Branch Live-Paths (ralph check / run)

The host-side branch of `ralph check` / `ralph run` is exercised end-to-end via
`setup_host_mocks` (see *Live-Path Testing*):

- **Clean push gate** — `test_check_host_push_gate_clean_live`: clean review
  triggers `git push` + `beads-push`, resets `iteration_count`.
- **Clarify stops push** — `test_check_host_clarify_stops_push_live`:
  pre-existing `ralph:clarify` bead suppresses the push and surfaces inline
  `ralph msg` output.
- **Auto-iterate under cap** — `test_check_host_auto_iterate_live`: new fix-up
  beads bump `iteration_count` and `exec ralph run -s <label>`.
- **Iteration cap escalation** — `test_check_host_iteration_cap_live`: at the
  cap, escalate by adding `ralph:clarify` to the newest fix-up bead, skip push
  and the handoff.
- **Push-gate failure modes** — `test_check_host_push_failures_live`:
  detached HEAD, `git push` failure, `beads-push` failure each exit non-zero
  with the documented hint. (Top-level `check.sh` collapses all push-gate codes
  to `exit 1`; the hint text differentiates the failure mode.)
- **run → check handoff** — `test_run_to_check_handoff_live`: continuous
  `ralph run` exec's `ralph check --spec <label>` on the host post-wrapix.

### Concurrent Workflows

Multiple specs coexist with isolated per-label `state.json`. `--spec`/`-s` flags on
`todo`/`run`/`status`/`logs` override the `current` pointer; `current` itself is
written as plain text and read exactly once per invocation to tolerate mid-run
switches. Serial (pre-per-label) workflows remain supported.

### Spec Change Detection

`ralph todo` invokes a three-tier diff: (1) git diff against `base_commit`, (2)
molecule fallback from state, (3) README discovery + state reconstruction. `--since`
overrides the base. Uncommitted changes abort with a clear error. `base_commit`
advances on success and on post-sync warnings; `implementation_notes` is cleared.

### Beads Sync

Host pulls via `bd dolt pull` after `ralph todo` / `run` complete. Container pushes
via `bd dolt push` for `plan` / `todo` / `run` (once + continuous) / `check`. Failure
paths exercised: container commit failure, container push failure, host sync
failure (state reset), post-sync warning with recovery hints.

### Template Infrastructure

Companion templates inject variables and override partials. Re-pin hooks produce
condensed content of bounded size. Runtime dir is gitignored, propagates via env,
and cleans up on exit. Entrypoint merges ralph settings with host settings and
concatenates hook lists.

### Compaction Re-pin

`simulate_compact_event` runs the `SessionStart[compact]` hook through the same
JSON contract Claude uses, capturing `additionalContext`. After each ralph
invocation, the captured orientation must reflect current state (claimed issue,
molecule, label). Refresh-per-invocation is asserted — the helper, called after
two successive ralph commands, must observe updated content between them.

## Success Criteria

- [x] `nix run .#test` runs fast pure checks via `nix flake check`; `nix run .#test-ralph` runs the ralph integration suite
  [verify](../tests/ralph/run-tests.sh#test_mock_claude_exists)
- [x] Darwin tests skip gracefully on Linux
- [x] Ralph tests pass with mock claude (no real API calls)
  [verify](../tests/ralph/run-tests.sh#test_mock_claude_exists)
- [x] `ralph plan <label>` does setup AND interview (no separate `start` command)
  [verify](../tests/ralph/run-tests.sh#test_plan_flag_validation)
- [x] `ralph todo` creates molecule from spec
  [verify](../tests/ralph/run-tests.sh#test_run_closes_issue_on_complete)
- [x] `ralph run --once` processes single issue
  [verify](../tests/ralph/run-tests.sh#test_run_closes_issue_on_complete)
- [x] `ralph run` processes all issues continuously
  [verify](../tests/ralph/run-tests.sh#test_run_loop_processes_all)
- [x] Tests verify dependency-ordered task execution
  [verify](../tests/ralph/run-tests.sh#test_run_respects_dependencies)
- [x] Tests verify in_progress exclusion for parallel agents
  [verify](../tests/ralph/run-tests.sh#test_parallel_agent_simulation)
- [x] Tests verify error handling (missing signals, RALPH_BLOCKED, bad JSON)
  [verify](../tests/ralph/run-tests.sh#test_run_no_close_without_signal)
- [x] Tests verify config options affect behavior (spec.hidden, beads.priority, loop settings)
  [verify](../tests/ralph/run-tests.sh#test_config_data_driven)
- [x] Tests are deterministic and fast
  [judge](../tests/judges/ralph-tests.sh#test_deterministic_and_fast)
- [x] Test infrastructure split into `lib/` modules (assertions, fixtures, mock-claude, runner)
  [verify](../tests/ralph/run-tests.sh#test_isolated_beads_db)
- [x] JSON format support for declarative test scenarios
  [verify](../tests/ralph/run-tests.sh#test_run_handles_blocked_signal)
- [x] Shell format support for complex scenarios requiring custom logic
  [verify](../tests/ralph/run-tests.sh#test_run_loop_processes_all)
- [x] `ralph status` has coverage (mol position, awaiting, `--spec`, `--all`, missing state)
  [verify](../tests/ralph/run-tests.sh#test_status_mol_current_position)
- [x] `ralph logs` has coverage (error detection, `--all`, context, `--spec`, missing logfile)
  [verify](../tests/ralph/run-tests.sh#test_logs_error_detection)
- [x] `ralph sync` has coverage (fresh/backup/dry-run/partials, `--diff` filtering)
  [verify](../tests/ralph/run-tests.sh#test_sync_fresh)
- [x] `ralph check` has coverage (templates, push gate, iteration counter, auto-iterate, dolt sync)
  [verify](../tests/ralph/run-tests.sh#test_check_valid_templates)
- [x] `ralph use` / `msg` / `spec` / `tune` have coverage
  [verify](../tests/ralph/run-tests.sh#test_use_switches_active_workflow)
- [x] Per-label `state.json` isolation across concurrent workflows
  [verify](../tests/ralph/run-tests.sh#test_concurrent_state_isolation)
- [x] `ralph todo` detects spec changes via git diff (`base_commit`, `--since`, fallbacks)
  [verify](../tests/ralph/run-tests.sh#test_todo_git_diff)
- [x] Host-side `bd dolt pull` and container-side `bd dolt push` for plan/todo/run/check
  [verify](../tests/ralph/run-tests.sh#test_todo_dolt_push_in_container)
- [x] Companion templates, re-pin hooks, runtime dir, entrypoint settings merge
  [verify](../tests/ralph/run-tests.sh#test_companion_template_variable)
- [x] `simulate_compact_event` helper exercises `SessionStart[compact]` hook and returns `additionalContext`
  [verify](../tests/ralph/run-tests.sh#test_simulate_compact_event_returns_additional_context)
- [x] Re-pin content is asserted after each ralph command (plan/todo/run/check) and reflects current state
  [verify](../tests/ralph/run-tests.sh#test_repin_content_after_plan)
- [x] Refresh-per-invocation is verified — orientation updates between successive ralph commands
  [verify](../tests/ralph/run-tests.sh#test_simulate_compact_event_refresh_per_invocation)
- [x] Full suite runs in <60s on dev hardware (measured: 29s / 928 assertions on 2026-04-24)
- [x] `init_beads` uses snapshot copy instead of per-test `bd init`
  [verify](../tests/ralph/run-tests.sh#test_init_beads_uses_snapshot)
- [x] `setup_test_env` uses batched fixture operations (single `mkdir -p`, bulk symlinks)
- [x] `RALPH_TEST_MAX_JOBS` default (16 host / 5 container) balances CPU/memory and Claude rate limits

## Out of Scope

- Actual Claude API integration tests
- Performance benchmarking of ralph itself (test-suite speed target is in-scope; see NFR §2)
- UI/UX improvements to ralph commands
- Changes to beads functionality
- Mocking `bd` (tests run against live bd — see NFR §6)
