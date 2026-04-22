# Orchestration

Automated verification, parallel dispatch, and observation for ralph.

## Problem Statement

`ralph run` currently trusts the agent's self-assessment: when it emits RALPH_COMPLETE, the bead closes immediately with no independent verification. Implementation errors compound silently across a background run. There is no mechanism for parallel work on independent beads, no automated retry with error context, and no way to observe a running application and create beads from detected issues.

This spec adds three capabilities:
1. **Post-epic review** — independent verification of completed work
2. **Parallel dispatch** — concurrent workers on independent beads
3. **Observation** — monitoring running services for errors and creating beads

Together these make `ralph run` trustworthy enough for fully unsupervised work.

## Requirements

### Functional

#### 1. `ralph check` Consolidation

The existing `ralph check` (template validation) gains a second mode for spec review. No-flags prints usage help.

- `ralph check -t` / `--templates` — existing template validation (current behavior)
- `ralph check -s <label>` / `--spec <label>` — post-epic review of completed work
- `ralph check -s <label> -t` — both

#### 2. Post-Epic Review (`ralph check -s`)

An independent reviewer agent assesses the full deliverable after an epic completes.

**Input context (in prompt):**
- Spec file (`specs/<label>.md`)
- Beads summary (titles and status only — reviewer reads full descriptions on demand)
- `base_commit` SHA (reviewer runs `git diff` / `git log` as needed)
- Molecule ID

Context pinning (the file set via `pinnedContext`, default `docs/README.md`) and companions are injected via standard partials. The reviewer has full codebase access inside the container — it reads implementation code, test files, `CLAUDE.md`, and related specs on demand rather than having them injected into the prompt.

**Reviewer behavior:**
- Runs inside a wrapix container with base profile
- Explores the codebase as needed (reads files, runs git commands, inspects tests)
- Assesses spec compliance, code quality, test adequacy, coherence
- Creates follow-up beads (via `bd create` + `bd mol bond`) for actionable fixes
- Flags ambiguous items with `bd human` for human decision
- Emits RALPH_COMPLETE when review is finished — the orchestrator compares the molecule's bead count before and after the review to determine pass (unchanged) or fail (new beads added)

**Template:** New `check.md` in `lib/ralph/template/`. Reuses partials: `context-pinning`, `spec-header`, `companions-context`, `exit-signals`. New variables: `BEADS_SUMMARY` (titles + status), `BASE_COMMIT`, `MOLECULE_ID`.

**Exit signals:** RALPH_COMPLETE, RALPH_BLOCKED, RALPH_CLARIFY.

#### 3. Review Cycle in `ralph run`

`ralph run -c` / `--check` auto-triggers review after the molecule reaches 100%.

**Cycle:**
1. `ralph run` works all beads to completion
2. Molecule hits 100% — triggers `ralph check -s <label>`
3. Reviewer creates follow-up beads → bonded to molecule
4. `ralph run` resumes processing new beads
5. Molecule hits 100% again → re-triggers review
6. Reviewer passes (no follow-up beads) → done

**Limits:** Max review cycles configurable in `config.nix` (default: 2). After max cycles, the loop stops and notifies the human.

When the reviewer emits RALPH_CLARIFY, the epic bead itself gets the `ralph:clarify` label with the question in its notes. The review cycle pauses until the human responds via `ralph msg`.

#### 4. `ralph msg` — Async Human Communication

Human interface for responding to agent questions. No subcommands — flat flag structure consistent with other ralph commands.

```
ralph msg                          # List all outstanding questions
ralph msg -s <label>               # List for specific spec
ralph msg -i <id>                  # Show specific question
ralph msg -i <id> "answer"         # Reply (answer as positional)
ralph msg -i <id> -d               # Dismiss without answering
```

**Flags:**
- `-s` / `--spec` — filter by spec label
- `-i` / `--id` — target specific question
- `-d` / `--dismiss` — dismiss without answering

**Behavior:**
- List mode shows a table: ID, spec, source (worker/reviewer/watcher), question
- Reply stores the answer in bead notes and removes `ralph:clarify` label — the run loop's next bead-selection iteration will pick it up
- Dismiss removes `ralph:clarify` label with a note that the agent should work around it
- Abstracts bead storage — today uses bead labels/notes, interface is ralph-level

#### 5. `ralph:clarify` Label

Replaces `awaiting:input` for beads waiting on human response. Used by implementation workers, the reviewer agent, and the watch agent. The run loop filters out beads with the `ralph:clarify` label when selecting the next bead to work. Each iteration re-queries, so when a human removes the label via `ralph msg`, the bead becomes eligible on the next pass. A notification is emitted when the label is first applied.

#### 6. Parallel Dispatch

When the dependency graph has multiple ready beads with no dependencies between them, `ralph run` can spawn multiple containers concurrently.

- `-p N` / `--parallel N` — concurrency limit (default: 1, preserving sequential behavior)
- Each parallel worker gets its own git worktree branch (`ralph/<label>/<bead-id>`)
- On completion, the orchestrator merges the worker's branch back to the main working branch
- On merge conflict, the bead reopens with conflict details
- Configurable in `config.nix`: `loop.parallel = 1` (flag overrides config)

#### 7. Model Diversity

Different phases can use different models. Configurable per-phase in `config.nix`:

```nix
{
  model.run = null;      # inherit from environment (current behavior)
  model.check = null;    # inherit from environment
  model.plan = null;     # inherit from environment
  model.todo = null;     # inherit from environment
  model.watch = null;    # inherit from environment
}
```

`null` means use whatever `ANTHROPIC_MODEL` is set to. Override per-phase for cost/speed tradeoffs.

#### 8. Retry with Context

When a worker fails (no RALPH_COMPLETE), retry automatically with error context.

- `loop.max-retries = 2` in `config.nix` (per bead, default 2)
- On failure, the error output from the previous attempt is injected into the next attempt's `run.md` context as `PREVIOUS_FAILURE` template variable
- After max retries, the bead gets `ralph:clarify` label with failure details, loop moves on

#### 9. `ralph watch` — Observation

An observation agent that monitors running services and creates beads for detected issues.

```
ralph watch -s <label>                    # Monitor for specific spec
ralph watch -s <label> --panes <panes>    # Specify tmux panes to observe
```

**Architecture:**
- Host runs a poll loop (no LLM — just a timer) that spawns a fresh contained Claude session each cycle
- Each session reads watch state from `state/<label>.watch.md`, captures new output since last observation, evaluates it, updates the state file, and exits
- Short-lived sessions avoid context compaction — state is persisted explicitly, not accumulated in-context
- `ralph watch` spawns each container with tmux and playwright MCP servers enabled — the agent uses these existing servers to observe logs and browser state, acting as the aggregator (not a new MCP layer)
- Does NOT fix bugs — observe, investigate, create beads. Fixing is `ralph run`'s job

**Watch state file** (`state/<label>.watch.md`): A markdown file the agent reads at session start and updates before exiting. The agent decides what to track — pane positions, known issues, investigation notes, etc. No enforced schema; the `watch.md` template instructs the agent to maintain it.

**Observation model:**
- Polls tmux panes and playwright sessions on a configurable interval
- The LLM intelligently evaluates log output in context — no hardcoded error patterns
- The spec provides domain knowledge for what's normal vs abnormal behavior
- Optional `ignore-patterns` in config as a user escape hatch for known noisy output

**Investigation flow (within a single watch session):**
1. Agent captures new output from tmux panes and/or playwright
2. Spots anomaly — reads more context, attempts reproduction if needed
3. Deduplication check against `known_issues` in watch state and existing beads (`bd list`)
4. Creates bead with title, description, reproduction steps, log snippets
5. Bonds bead to the molecule (`bd mol bond`) so `ralph run` can pick it up
6. Labels: `spec:<label>`, `profile:<X>`, `source:watch`
7. Updates watch state with new known issue

**Config:**
```nix
watch = {
  poll-interval = 30;       # seconds between host loop cycles (each cycle spawns one agent session)
  max-issues = 10;          # max beads created before pausing (across all sessions, reset on restart)
  ignore-patterns = [];     # optional noise filter (written to watch state for the agent to read)
};
```

The host loop tracks total beads created by counting `source:watch` labeled beads in the molecule. When `max-issues` is reached, the host loop stops spawning new sessions and emits a notification. The human can restart with `ralph watch` to reset the counter.

**Template:** New `watch.md` in `lib/ralph/template/`. Instructs the agent to monitor contextually based on the spec, not pattern-match.

#### 10. Notification Integration

Uses existing `wrapix-notify`/`wrapix-notifyd` for key events:

- `ralph:clarify` — "Input needed for \<bead title\>"
- Review cycle complete (pass) — "Review passed for \<label\>"
- Review cycle complete (follow-up beads) — "Review found N issues for \<label\>"
- Max review cycles reached — "Review limit reached for \<label\>"
- `ralph watch` — "New issue detected: \<bead title\>"

### Non-Functional

1. **Separation of concerns** — Implementer, reviewer, and observer are distinct agent roles with distinct context
2. **Composable** — Each capability works standalone (`ralph check -s`, `ralph watch`) or integrated (`ralph run -c`)
3. **Backward compatible** — Default behavior unchanged (`-p 1`, no `-c`, no `ralph watch`)
4. **Cost-aware** — Model diversity and concurrency limits prevent runaway spend
5. **Async-first** — No TTY required; all human interaction via `ralph msg`

## Affected Files

| File | Role |
|------|------|
| `lib/ralph/cmd/ralph.sh` | Add `watch` and `msg` cases to dispatcher |
| `lib/ralph/cmd/check.sh` | Extend with `-s`/`-t` flag routing |
| `lib/ralph/cmd/run.sh` | Add `-c`/`--check`, `-p`/`--parallel`, retry logic, worktree management |
| `lib/ralph/cmd/watch.sh` | New — observation loop |
| `lib/ralph/cmd/msg.sh` | New — async human communication |
| `lib/ralph/cmd/util.sh` | Add worktree helpers, merge logic, `ralph:clarify` label management |
| `lib/ralph/template/check.md` | New — reviewer agent prompt |
| `lib/ralph/template/watch.md` | New — observation agent prompt |
| `lib/ralph/template/run.md` | Add `PREVIOUS_FAILURE` variable |
| `lib/ralph/template/default.nix` | Register new templates and variables |
| `lib/ralph/template/config.nix` | Add `watch`, `model`, `loop.parallel`, `loop.max-retries`, `loop.max-reviews` |
| `lib/sandbox/default.nix` | Support model override per container |

## Success Criteria

- [ ] `ralph check` with no flags prints usage help
  [verify](tests/orchestration-test.sh::test_check_no_flags_prints_help)
- [ ] `ralph check -t` validates templates (existing behavior preserved)
  [verify](tests/orchestration-test.sh::test_check_templates)
- [ ] `ralph check -s <label>` spawns reviewer agent in wrapix container
  [verify](tests/orchestration-test.sh::test_check_spec_runs_in_container)
- [ ] Reviewer receives spec, beads summary (titles + status), base_commit, and molecule ID — explores codebase on demand
  [judge](tests/judges/orchestration.sh::test_reviewer_context)
- [ ] Reviewer creates follow-up beads for actionable issues
  [verify](tests/orchestration-test.sh::test_reviewer_creates_followup_beads)
- [ ] Reviewer flags ambiguous items with `bd human`
  [verify](tests/orchestration-test.sh::test_reviewer_flags_human_items)
- [ ] `ralph run -c` triggers review after molecule reaches 100%
  [verify](tests/orchestration-test.sh::test_run_check_triggers_review)
- [ ] Review cycle processes follow-up beads then re-reviews
  [verify](tests/orchestration-test.sh::test_review_cycle_loops)
- [ ] Review cycle respects `max-reviews` limit
  [verify](tests/orchestration-test.sh::test_review_cycle_max_limit)
- [ ] `ralph msg` lists all outstanding questions
  [verify](tests/orchestration-test.sh::test_msg_list)
- [ ] `ralph msg -s <label>` filters by spec
  [verify](tests/orchestration-test.sh::test_msg_list_by_spec)
- [ ] `ralph msg -i <id> "answer"` stores reply and removes `ralph:clarify` label
  [verify](tests/orchestration-test.sh::test_msg_reply)
- [ ] `ralph msg -i <id> -d` dismisses without answering
  [verify](tests/orchestration-test.sh::test_msg_dismiss)
- [ ] `ralph:clarify` label replaces `awaiting:input` in the run loop
  [verify](tests/orchestration-test.sh::test_clarify_label)
- [ ] `ralph run -p N` spawns N concurrent workers for independent beads
  [verify](tests/orchestration-test.sh::test_parallel_dispatch)
- [ ] Parallel workers use git worktrees for isolation
  [verify](tests/orchestration-test.sh::test_parallel_worktrees)
- [ ] Worktree branches merge back on completion
  [verify](tests/orchestration-test.sh::test_worktree_merge)
- [ ] Merge conflicts reopen the bead with conflict details
  [verify](tests/orchestration-test.sh::test_merge_conflict_handling)
- [ ] `config.nix` model overrides are passed to container
  [verify](tests/orchestration-test.sh::test_model_override)
- [ ] Failed steps retry with `PREVIOUS_FAILURE` context
  [verify](tests/orchestration-test.sh::test_retry_with_context)
- [ ] Retries respect `max-retries` limit then label `ralph:clarify`
  [verify](tests/orchestration-test.sh::test_retry_max_limit)
- [ ] `ralph watch -s <label>` monitors tmux panes and creates beads
  [verify](tests/orchestration-test.sh::test_watch_creates_beads)
- [ ] Watch agent deduplicates against existing beads
  [verify](tests/orchestration-test.sh::test_watch_deduplication)
- [ ] Watch agent respects `max-issues` limit
  [verify](tests/orchestration-test.sh::test_watch_max_issues)
- [ ] Watch-created beads have `source:watch` label
  [verify](tests/orchestration-test.sh::test_watch_bead_labels)
- [ ] `ralph run` picks up watch-created beads automatically
  [verify](tests/orchestration-test.sh::test_run_picks_up_watch_beads)
- [ ] Notifications fire for `ralph:clarify`, review results, and watch detections
  [verify](tests/orchestration-test.sh::test_notifications)
- [ ] `check.md` template instructs reviewer to assess tests, code, and spec compliance holistically
  [judge](tests/judges/orchestration.sh::test_check_template_quality)
- [ ] `watch.md` template instructs agent to observe contextually, not pattern-match
  [judge](tests/judges/orchestration.sh::test_watch_template_quality)
- [ ] `run.md` template includes `PREVIOUS_FAILURE` variable for retry context
  [judge](tests/judges/orchestration.sh::test_run_template_retry_context)

## Out of Scope

- CI workflow definitions (GitHub Actions, etc.)
- PR creation automation
- Event-based MCP monitoring (push instead of poll — future tmux/playwright enhancement)
- Cross-workflow coordination (multiple `ralph watch` instances)
- Workspace locking or mutex between parallel workers beyond git merge
- Automatic model selection based on task complexity
