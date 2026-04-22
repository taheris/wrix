# Live Specs

Specs become queryable, verifiable, and observable — not just static documentation.

## Problem Statement

Specs in `specs/` are static markdown files. Once written, there's no structured way to ask "does this feature actually work?" or "what needs my attention?" You have to mentally join `bd show`, `git log`, test output, and the spec itself to get a full picture.

Meanwhile, `ralph status` shows molecule progress but not whether the implementation is correct, `ralph logs` shows errors but not what's blocked on human input, and there's no live view when `ralph run` is active.

The "frontend dissolves" insight: instead of building dashboards and status pages, make the spec itself the interface. Queries, verification, and observation are commands that read structured annotations in specs.

## Requirements

### Functional

1. **Spec annotations** — Success criteria in specs support `[verify]` and `[judge]` links:
   ```markdown
   - [ ] Notification appears within 2s
     [verify](../tests/notify-test.sh#test_notification_timing)
   - [ ] Clear visibility into current state
     [judge](../tests/judges/notify.sh#test_clear_visibility)
   - [ ] Works on both Linux and macOS
   ```
   - `[verify](path#function)` points to a shell test (exit code pass/fail)
   - `[judge](path#function)` points to an LLM evaluation rubric
   - Criteria with no annotation are unannotated
   - Links are clickable in editors and GitHub (standard markdown link syntax)

2. **`ralph spec`** — Fast annotation index across all spec files:
   ```
   Ralph Specs
   ============================
     notifications.md     3 verify, 1 judge, 2 unannotated
     sandbox.md           0 verify, 0 judge, 5 unannotated
     ralph-workflow.md    2 verify, 0 judge, 8 unannotated

   Total: 5 verify, 1 judge, 15 unannotated
   ```
   - Reads spec files and parses annotations; no test execution or LLM calls
   - `--verbose`: expand to per-criterion detail showing each criterion and its annotation type

3. **`ralph spec --verify` / `-v`** — Run all `[verify]` tests across all spec files by default:
   ```
   Ralph Verify: notifications (wx-q6x)
   ======================================
     [PASS] Notification appears within 2s
            tests/notify-test.sh::test_notification_timing (exit 0)
     [SKIP] Clear visibility into current state (judge only)

   Ralph Verify: live-specs (wx-a13n)
   ======================================
     [PASS] Annotations parse correctly
     [FAIL] ralph spec --verify runs shell tests

   Summary: 2 passed, 1 failed, 1 skipped (2 specs)
   ```
   - Iterates all spec files in `specs/` (excluding README.md) by default
   - Specs with no success criteria are silently skipped
   - Results grouped by spec with per-spec headers
   - Ends with a cross-spec summary line
   - Exits non-zero if any spec has at least one failure
   - Runs on the host (user is responsible for having required tools)

4. **`ralph spec --judge` / `-j`** — Run all `[judge]` evaluations across all spec files by default:
   ```
   Ralph Judge: notifications (wx-q6x)
   =====================================
     [SKIP] Notification appears within 2s (verify only)
     [PASS] Clear visibility into current state
            "ralph status displays progress %, per-issue status,
             and blocked/awaiting indicators"

   Summary: 1 passed, 0 failed, 1 skipped (1 spec)
   ```
   - Same multi-spec iteration and grouping as `--verify`
   - Runs on the host; invokes LLM with source files + criterion

5. **`ralph spec --all` / `-a`** — Run both verify and judge checks (shorthand for `--verify --judge`). Short flags compose: `ralph spec -vj` is equivalent to `ralph spec --all`.

6. **`--spec` / `-s` flag** — Filter verify/judge/all to a single spec:
   - `ralph spec --verify --spec notifications` runs only against `specs/notifications.md`
   - The name matches the spec filename without the `.md` extension
   - When `--spec` is provided, output matches single-spec format (no cross-spec summary)

7. **Short flags** — `-v` for `--verify`, `-j` for `--judge`, `-a` for `--all`, `-s` for `--spec`. The `-v` flag is reassigned from `--verbose`; `--verbose` has no short flag.

8. **Judge test infrastructure** — Judge tests live in `tests/judges/` and define rubrics:
   ```bash
   test_clear_visibility() {
     judge_files "lib/ralph/cmd/status.sh"
     judge_criterion "Output includes progress percentage, per-issue status indicators (done/running/blocked/awaiting), and dependency information"
   }
   ```
   - `judge_files` specifies which source files the LLM reads
   - `judge_criterion` specifies what the LLM evaluates
   - The runner invokes an LLM with files + criterion, returns PASS/FAIL + short reasoning

9. **`ralph sync --deps`** — Print required nix packages for verify and judge tests:
   - Scans annotations in the current spec
   - Determines what tools/packages the test files need
   - Outputs a list suitable for `nix shell` or profile construction

10. **`ralph status --watch` / `-w`** — Auto-refreshing live view:
   - Top pane: molecule progress (refreshes periodically)
   - Bottom pane: live tail of agent output if `ralph run` is active
   - Works standalone (shows status + recent activity even when nothing is running)
   - Uses tmux split for layout
   - **Requires tmux** — errors with a clear message if not in a tmux session
   - Individual underlying commands (`ralph status`, `ralph logs`) remain usable outside tmux

11. **Awaiting input convention** — Tracker-agnostic label for human-blocked items:
   - When agent emits `RALPH_CLARIFY`, orchestrator adds `ralph:clarify` label to bead
   - Question stored in bead notes
   - `ralph status` surfaces awaiting items distinctly:
     ```
     [awaiting] wx-q6x-10  Cross-platform CI
                  "Should CI use GitHub Actions or Buildkite?" (2h ago)
     ```
   - `ralph run` skips beads with `ralph:clarify` label (not truly ready)
   - Human answers via `ralph msg`, label removed, bead becomes ready again
   - Convention works with any tracker that supports labels/tags

### Non-Functional

1. **Fast default** — `ralph spec` and `ralph status` with no flags must be instant (no tests, no LLM calls)
2. **Clickable links** — `[verify]` and `[judge]` annotations use standard markdown link syntax, rendering as clickable links in GitHub, VS Code, and terminal markdown viewers
3. **Tracker-agnostic** — `ralph:clarify` convention uses labels, not custom status fields, so it works with any issue tracker
4. **Parseable annotations** — `[verify](path#fn)` and `[judge](path#fn)` patterns are easy to extract programmatically from spec markdown
5. **Host execution** — `ralph spec --verify` and `ralph spec --judge` run on the host, not in wrapix containers; users are responsible for having required tools available (use `ralph sync --deps` to check)

## Design

### Annotation parsing

Scan success criteria sections for `[verify](...)` and `[judge](...)` links on lines following `- [ ]` or `- [x]` items. Extract path and optional function name from the link target (split on `#`, or `::` for legacy compatibility). Spec-relative paths (starting with `../`) are resolved to repo-root-relative paths using the spec file's directory.

### Multi-spec iteration

When `--verify`, `--judge`, or `--all` is used without `--spec`:
- Iterate all `specs/*.md` files
- For each spec, look up the molecule ID from the pinned-context file (`docs/README.md` by default) for the header
- Skip specs with no success criteria silently
- Run tests per-spec, grouping output with per-spec headers
- Track cumulative pass/fail/skip counts and spec count
- Print cross-spec summary: `Summary: X passed, Y failed, Z skipped (N specs)`
- Exit non-zero if any failure occurred across any spec

When `--spec NAME` is provided:
- Resolve to `specs/NAME.md`
- Run only that spec's tests
- Use single-spec output format (no cross-spec summary)

### Verify runner

For each `[verify]` annotation:
- Parse `path::function` from the link
- If function specified: invoke the test file with the function name as argument
- If file only: run the entire test file
- PASS on exit 0, FAIL on non-zero
- Capture stdout/stderr for the report

### Judge runner

For each `[judge]` annotation:
- Parse `path::function` from the link
- Source the judge test file and call the function to get `judge_files` and `judge_criterion`
- Read the specified source files
- Invoke LLM with: source file contents + criterion text
- LLM returns PASS/FAIL + short reasoning
- Display reasoning in output

### Watch mode

- Create tmux split: top pane runs `watch -n5 ralph status`, bottom pane tails agent log
- If no active `ralph run` session: bottom pane shows recent git log + last errors
- Standalone-capable: useful even when nothing is running
- Errors immediately if `$TMUX` is not set

### Awaiting input flow

```
ralph run (orchestrator)
  → agent emits RALPH_CLARIFY: "question text"
  → orchestrator: add_clarify_label <id> "question text"
    (adds ralph:clarify label, stores question in notes, emits notification)
  → orchestrator skips this bead in future iterations
  → human: ralph msg -i <id> "answer text"
    (stores answer, removes ralph:clarify label)
  → bead becomes ready again
```

## Command Summary

Updated `ralph` command structure with live-specs additions:

```
Spec-Driven Workflow Commands:
  plan            (unchanged)
  todo            (unchanged)
  run             Execute work items (+ ralph:clarify handling)
    --once/-1       Execute single issue then exit
    --profile=X     Override container profile
  status          Show current workflow state
    --watch/-w      Live tmux view (requires tmux)
  spec            Query spec annotations
    --verify/-v     Run [verify] shell tests (all specs by default)
    --judge/-j      Run [judge] LLM evaluations (all specs by default)
    --all/-a        Run both verify and judge
    --spec/-s NAME  Filter to a single spec file
    --verbose       Show per-criterion detail (no short flag)

Template Commands:
  check           (unchanged)
  sync            Update local templates from packaged
    --diff          Show local template changes vs packaged
    --deps          Show required nix packages for verify/judge tests
  tune            (unchanged)

Utility Commands:
  logs            (unchanged, extended independently)
  edit            (unchanged)
```

## Affected Files

| File | Role |
|------|------|
| `lib/ralph/cmd/ralph.sh` | Add `spec` subcommand dispatch |
| `lib/ralph/cmd/spec.sh` | New: annotation parsing, verify/judge runners, index display |
| `lib/ralph/cmd/status.sh` | Add `--watch` flag, surface awaiting items in default output |
| `lib/ralph/cmd/run.sh` | Awaiting:input label handling in orchestrator loop |
| `lib/ralph/cmd/sync.sh` | Add `--deps` flag |
| `lib/ralph/cmd/util.sh` | Annotation parsing helpers, judge runner utilities |
| `specs/*.md` | Add `[verify]` and `[judge]` annotations to existing success criteria |
| `tests/judges/` | New directory for judge rubric test files |

## Success Criteria

- [ ] `[verify]` and `[judge]` annotations parse correctly from spec success criteria
  [verify](../tests/ralph/run-tests.sh#test_parse_spec_annotations)
- [ ] `ralph spec` lists all spec files with annotation counts (verify/judge/unannotated)
  [judge](../tests/judges/live-specs.sh#test_spec_index_output)
- [ ] `ralph spec --verbose` shows per-criterion detail
  [judge](../tests/judges/live-specs.sh#test_spec_verbose_output)
- [ ] `ralph spec --verify` runs shell tests across all specs and reports PASS/FAIL/SKIP
  [judge](../tests/judges/live-specs.sh#test_spec_verify_runner)
- [ ] `ralph spec --judge` invokes LLM with rubric across all specs and reports PASS/FAIL/SKIP
  [judge](../tests/judges/live-specs.sh#test_spec_judge_runner)
- [ ] `ralph spec --all` runs both verify and judge
  [judge](../tests/judges/live-specs.sh#test_spec_all_flag)
- [ ] Criteria with no annotation show as SKIP in verify/judge output
  [verify](../tests/ralph/run-tests.sh#test_parse_spec_annotations)
- [ ] `ralph spec` and `ralph status` with no flags remain instant (no test/LLM execution)
  [judge](../tests/judges/live-specs.sh#test_spec_instant_default)
- [ ] `ralph status --watch` creates tmux split with auto-refresh
  [judge](../tests/judges/live-specs.sh#test_status_watch_tmux)
- [ ] `ralph status --watch` errors clearly when not in tmux
  [judge](../tests/judges/live-specs.sh#test_status_watch_no_tmux)
- [ ] `ralph status --watch` works standalone (no active ralph run required)
  [judge](../tests/judges/live-specs.sh#test_status_watch_standalone)
- [ ] `RALPH_CLARIFY` in orchestrator adds `ralph:clarify` label to bead
  [verify](../tests/ralph/run-tests.sh#test_run_handles_clarify_signal)
- [ ] `ralph run` skips beads with `ralph:clarify` label
  [judge](../tests/judges/live-specs.sh#test_run_skips_awaiting)
- [ ] `ralph status` displays awaiting items with question text and age
  [judge](../tests/judges/live-specs.sh#test_status_awaiting_display)
- [ ] Judge test files define rubrics via `judge_files` and `judge_criterion`
  [judge](../tests/judges/live-specs.sh#test_judge_rubric_format)
- [ ] `ralph sync --deps` lists required nix packages for current spec's tests
  [verify](../tests/ralph/run-tests.sh#test_sync_deps_basic)
- [ ] Annotations are clickable links in GitHub and VS Code
  [judge](../tests/judges/live-specs.sh#test_clickable_links)
- [ ] `ralph spec --verify` with no `--spec` flag runs verify tests across all spec files
  [judge](../tests/judges/live-specs.sh#test_spec_verify_all_specs)
- [ ] `ralph spec --judge` with no `--spec` flag runs judge tests across all spec files
  [judge](../tests/judges/live-specs.sh#test_spec_judge_all_specs)
- [ ] `ralph spec --all` with no `--spec` flag runs both across all spec files
  [judge](../tests/judges/live-specs.sh#test_spec_all_all_specs)
- [ ] `ralph spec --verify --spec notifications` runs verify tests only for `specs/notifications.md`
  [judge](../tests/judges/live-specs.sh#test_spec_filter_single)
- [ ] `ralph spec -v` is equivalent to `ralph spec --verify`
  [verify](../tests/ralph/run-tests.sh#test_spec_short_flag_v)
- [ ] `ralph spec -j` is equivalent to `ralph spec --judge`
  [verify](../tests/ralph/run-tests.sh#test_spec_short_flag_j)
- [ ] `ralph spec -a` is equivalent to `ralph spec --all`
  [verify](../tests/ralph/run-tests.sh#test_spec_short_flag_a)
- [ ] `ralph spec -s notifications` is equivalent to `ralph spec --spec notifications`
  [verify](../tests/ralph/run-tests.sh#test_spec_short_flag_s)
- [ ] `-v` no longer maps to `--verbose`; `--verbose` has no short flag
  [verify](../tests/ralph/run-tests.sh#test_spec_verbose_no_short_v)
- [ ] Short flags compose: `ralph spec -vj` is equivalent to `ralph spec --all`
  [verify](../tests/ralph/run-tests.sh#test_spec_short_compose)
- [ ] Multi-spec output groups results by spec with per-spec headers
  [judge](../tests/judges/live-specs.sh#test_spec_grouped_output)
- [ ] Multi-spec output ends with a summary line including total pass/fail/skip and spec count
  [judge](../tests/judges/live-specs.sh#test_spec_summary_line)
- [ ] Exit code is non-zero if any spec has a failure in multi-spec mode
  [verify](../tests/ralph/run-tests.sh#test_spec_nonzero_exit)
- [ ] Specs with no success criteria are silently skipped in multi-spec mode
  [verify](../tests/ralph/run-tests.sh#test_spec_skip_empty)

## Out of Scope

- Deprecating `ralph logs` (stays as-is, extended independently)
- Auto-updating `- [ ]` to `- [x]` in specs based on verify/judge results
- Web-based dashboard or GUI
- Judge model selection (use whatever model ralph is configured with)
- Running verify/judge inside wrapix containers (host execution only)

## Updates

# New Requirements for live-specs

## Requirements

1. **`ralph spec --verify/--judge/--all` run across all specs by default** — Instead of only running against the current spec (from `current.json`), these flags now scan and execute tests across all spec files in `specs/`. Results are grouped by spec with a per-spec header and a cross-spec summary line at the end.

2. **`--spec`/`-s` flag to filter to a single spec** — `ralph spec --verify --spec notifications` runs only against `specs/notifications.md`. The name matches the spec filename without the `.md` extension. When `--spec` is provided, behavior matches current single-spec output format (with the addition of the spec name in the header).

3. **Short flags for verify/judge/all** — Add `-v` for `--verify`, `-j` for `--judge`, `-a` for `--all`. These are composable (e.g., `ralph spec -vj` is equivalent to `ralph spec --all`).

4. **Drop `-v` short flag for `--verbose`** — Currently `-v` maps to `--verbose`. Reassign `-v` to `--verify`. `--verbose` has no short flag.

5. **Grouped output format** — When running across all specs, output groups results by spec file with headers, then ends with a cross-spec summary:
   ```
   Ralph Verify: notifications (wx-q6x)
   ======================================
     [PASS] Notification appears within 2s
     [SKIP] Clear visibility into current state (judge only)

   Ralph Verify: live-specs (wx-a13n)
   ======================================
     [PASS] Annotations parse correctly
     [FAIL] ralph spec --verify runs shell tests

   Summary: 2 passed, 1 failed, 1 skipped (2 specs)
   ```

6. **Non-zero exit code on any failure** — When running across all specs, exit non-zero if any spec has at least one failure.

## Success Criteria

- [ ] `ralph spec --verify` with no `--spec` flag runs verify tests across all spec files
- [ ] `ralph spec --judge` with no `--spec` flag runs judge tests across all spec files
- [ ] `ralph spec --all` with no `--spec` flag runs both across all spec files
- [ ] `ralph spec --verify --spec notifications` runs verify tests only for `specs/notifications.md`
- [ ] `ralph spec -v` is equivalent to `ralph spec --verify`
- [ ] `ralph spec -j` is equivalent to `ralph spec --judge`
- [ ] `ralph spec -a` is equivalent to `ralph spec --all`
- [ ] `ralph spec -s notifications` is equivalent to `ralph spec --spec notifications`
- [ ] `-v` no longer maps to `--verbose`; `--verbose` has no short flag
- [ ] Short flags compose: `ralph spec -vj` is equivalent to `ralph spec --all`
- [ ] Multi-spec output groups results by spec with per-spec headers
- [ ] Multi-spec output ends with a summary line including total pass/fail/skip and spec count
- [ ] Exit code is non-zero if any spec has a failure in multi-spec mode
- [ ] Specs with no success criteria are silently skipped in multi-spec mode

## Affected Files

| File | Change |
|------|--------|
| `lib/ralph/cmd/spec.sh` | Refactor `run_spec_tests` to iterate all specs by default; add `--spec`/`-s` flag; reassign `-v` to `--verify`; add `-j`, `-a` short flags; grouped output with cross-spec summary |

## Implementation Notes (non-spec)

- Create separate beads for each spec file to run `--all` tests and fix any failures found
