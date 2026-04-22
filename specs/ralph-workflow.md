# Ralph Workflow

Spec-driven AI orchestration for feature development.

## Problem Statement

AI coding assistants work best with:
- Clear specifications before implementation
- Focused, single-issue work sessions
- Progress tracking across sessions
- Consistent prompts and context

Ralph provides a structured workflow that guides AI through spec creation, issue breakdown, and implementation.

## Requirements

### Functional

1. **Spec Interview** — `ralph plan` initializes feature and conducts requirements gathering
2. **Plan Modes** — `ralph plan` requires exactly one mode flag:
   - `-n/--new`: New spec in `specs/`
   - `-h/--hidden`: New spec in `state/` (not committed)
   - `-u/--update`: Refine existing spec (combinable with `-h`)
3. **Molecule Creation** — `ralph todo` converts specs to beads molecules
4. **Issue Work** — `ralph run` processes issues (single with `--once`, continuous by default)
5. **Progress Tracking** — `ralph status` shows molecule progress
6. **Log Access** — `ralph logs` finds errors and shows context
7. **Template Validation** — `ralph check` validates all templates and partials
8. **Template Tuning** — `ralph tune` edits templates (interactive or integration mode)
9. **Template Sync** — `ralph sync` updates local templates (use `--diff` to preview changes)
10. **Concurrent Workflows** — Per-label state files (`state/<label>.json`) replace singleton `state/current.json`, enabling multiple ralph workflows simultaneously
11. **Spec Switching** — `ralph use <name>` sets the active workflow; `--spec <name>` flag on `todo`, `run`, `status`, `logs` targets a specific workflow
12. **Companion Content** — Specs can declare companion directories whose `manifest.md` is injected into templates, giving the LLM awareness of related content it can read on demand
13. **Git-Based Spec Diffing** — `ralph todo` uses `base_commit` in state JSON and `git diff` to detect spec changes, replacing `state/<label>.md` intermediary
14. **Container Bead Sync** — Container-executed commands (`plan`, `todo`, `run --once`, `check`) run `bd dolt push` inside the container after `RALPH_COMPLETE` before exit, then `bd dolt pull` on the host after container exits
15. **Cross-Machine State Recovery** — `ralph todo` discovers molecule IDs from the pinned-context file (`docs/README.md` by default) when no local state file exists, reconstructing `state/<label>.json` to avoid duplicate molecule creation
16. **Post-Sync Verification (informational)** — After `bd dolt pull` on the host, `ralph todo` checks whether task count increased and emits a warning with recovery hints if not; `base_commit` always advances on `RALPH_COMPLETE`
17. **Compaction Re-Pin** — Container-executed Claude sessions register a `SessionStart` hook with matcher `"compact"` that re-injects a condensed orientation (label, spec path, exit signals, command-specific context) after auto-compaction, so the model retains orientation without re-pinning the full spec body
18. **Invariant-Clash Awareness** — `ralph plan -u` (during spec updates) and `ralph check` (post-loop review) detect when a proposed change clashes with an existing invariant and surface the clash for a human decision instead of silently choosing a path. The detection is LLM-judgment biased toward asking. The reviewer proposes *contextual* options tailored to the specific clash (not a fixed menu), guided by the three-paths principle: preserve the invariant, keep the change on top of the invariant inelegantly, or change the invariant
19. **Post-Loop Review & Push Gate** — `ralph run` auto-invokes `ralph check` when the molecule reaches completion. `ralph run` commits work per-bead during the loop but does NOT push; push is owned by `ralph check` and only happens when check emits `RALPH_COMPLETE` with no new beads created. If check creates fix-up beads without a clarify, it auto-invokes `ralph run` to work them, then re-runs itself. Iteration continues until clean RALPH_COMPLETE (→ push), `ralph:clarify` (→ stop, wait for user), or the `loop.max-iterations` cap is reached (→ escalate via `ralph:clarify`)
20. **Planning Interview Modes** — During `ralph plan -n` / `plan -u`, the user can request two structured sub-modes by saying phrases like "one by one" (walk open design questions individually with suggested defaults) or "polish the spec" (end-of-session read-through for readability, consistency, and ambiguities). Phrase matching is loose — variations like "let's go through one by one" or "polish this spec" also trigger the respective mode

### Non-Functional

1. **Context Efficiency** — Each step starts with minimal, focused context
2. **Resumable** — Work can stop and resume across sessions
3. **Observable** — Clear visibility into current state and progress via molecules
4. **Validated** — Templates statically checked at build time and after edits
5. **Isolated** — Claude-calling commands run inside wrapix containers for security and reproducibility

## Commands

### `ralph plan`

```bash
ralph plan -n <label>           # New spec in specs/
ralph plan -h <label>           # New spec in state/ (hidden)
ralph plan -u <label>           # Update existing spec in specs/
ralph plan -u -h <label>        # Update existing spec in state/
```

**Flags (exactly one mode required, except -u and -h can combine):**

| Flag | Location | Mode | Template |
|------|----------|------|----------|
| `-n/--new` | `specs/` | create | `plan-new.md` |
| `-h/--hidden` | `state/` | create | `plan-new.md` |
| `-u/--update` | auto-detect | update | `plan-update.md` |
| `-u -h` | `state/` | update | `plan-update.md` |

**Validation:**
- `-u/--update`: Error if spec doesn't exist at expected location
- No flag or invalid combination: Error with usage help

**Behavior:**
- Creates `state/<label>.json` with feature metadata (per-label, not singleton)
- Writes label to `state/current` (plain text, no extension) — the active planning target becomes the default
- Launches wrapix container with base profile
- Runs spec interview using appropriate template
- **New mode**: Writes spec to target location (`specs/` or `state/`)
- **Update mode**: LLM edits `specs/<label>.md` directly during the interview; commits changes at end of session (git-tracked specs only; hidden specs just save file)
- Outputs `RALPH_COMPLETE` when done
- Container runs `bd dolt push` after `RALPH_COMPLETE` (syncs beads to Dolt remote)
- Host runs `bd dolt pull` after container exits (receives synced beads)

### `ralph todo`

```bash
ralph todo                  # Operate on current spec (from state/current)
ralph todo --spec <name>    # Operate on named spec
ralph todo -s <name>        # Short form
ralph todo --since <commit> # Force git-diff mode from specific commit
```

Launches wrapix container with base profile. Reads `state/<label>.json` (resolved via `--spec` flag or `state/current`). Uses four-tier detection to determine mode:

1. **Tier 1 (git diff)**: `base_commit` exists in state JSON → `git diff <base_commit> HEAD -- <spec_path>` (fast, precise)
2. **Tier 2 (molecule-based)**: No `base_commit` but molecule in state JSON → LLM compares spec against existing task descriptions
3. **Tier 3 (README discovery)**: No `state/<label>.json` exists → look up molecule ID from the pinned-context file (`docs/README.md` by default) Beads column, validate with `bd show`, reconstruct state file, then proceed as tier 2
4. **Tier 4 (new)**: No state file AND no molecule in README (or README molecule is stale/invalid) → full spec decomposition from `specs/<label>.md`

**Flags:**
- `--since <commit>` forces tier 1 with the given commit, skipping tier 2/3/4; errors if commit is invalid

**Constraints:**
- Requires spec changes to be committed — errors if uncommitted changes detected in spec file
- If tier 1 finds no changes (empty diff), exits early: "No spec changes since last task creation"
- When `base_commit` is orphaned (rebase/amend), detected via `git merge-base --is-ancestor`, falls back to tier 2
- Hidden spec updates (`-u -h`) use tier 2 since hidden specs are not in git
- Tier 3 reconstructs `state/<label>.json` with: label, spec_path (`specs/<label>.md`), molecule (from README), no base_commit, empty companions array

**Profile assignment:** The LLM analyzes each task's requirements and assigns appropriate `profile:X` labels based on implementation needs (e.g., tasks touching `.rs` files get `profile:rust`). This happens per-task, not per-spec.

Stores molecule ID in `state/<label>.json`. On `RALPH_COMPLETE`, atomically stores `HEAD` as `base_commit` and clears `implementation_notes` (see Implementation Notes section for lifecycle rationale). Host-side verification checks that tasks synced correctly but is informational only — it does not block `base_commit` advancement or workflow progression.

**Container bead sync:** After RALPH_COMPLETE inside the container, `todo.sh` runs `bd dolt push` before the container exits. On the host side, `todo.sh` then runs `bd dolt pull` to receive the synced beads. This two-step sync (push inside container → pull on host) ensures beads created in the container's isolated `.beads/` database reach the host.

**Post-completion verification (host-side, informational):** Before launching the container, the host-side `todo.sh` counts beads with the `spec:<label>` label via `bd list -l spec:<label>` (0 if none exist yet). After the container exits with RALPH_COMPLETE and `bd dolt pull` completes, the host re-counts using the same label query. If the count did not increase, it emits a warning:
```
Warning: RALPH_COMPLETE but no new tasks detected after sync.
  If bd dolt push failed above, tasks may not have synced.
  Check: bd list -l spec:<label>
  To re-run: ralph todo --since <previous_base_commit>
```

This warning is informational — it does **not** block the workflow:
- `base_commit` is still updated (RALPH_COMPLETE is authoritative)
- Spec/README changes are still committed
- Exit code 0

If sync genuinely failed, the user will notice when `ralph run` finds no tasks. Recovery is straightforward: edit `base_commit` in `state/<label>.json` or re-run with `ralph todo --since <commit>`.

### `ralph run`

```bash
ralph run                   # Continuous mode: process all issues until complete
ralph run --once            # Single-issue mode: process one issue then exit
ralph run -1                # Alias for --once
ralph run --profile=rust    # Override profile (applies to all iterations)
ralph run --spec <name>     # Operate on named spec
ralph run -s <name>         # Short form
```

**Spec resolution:** Reads the spec label once at startup (from `--spec` flag or `state/current`). The label is held in memory for the duration of the run — switching `state/current` via `ralph use` does not affect a running `ralph run`. Does NOT update `state/current` during execution.

**Default (continuous) mode** — Runs on host as orchestrator:
- Queries for next ready issue from molecule (skips beads with `ralph:clarify` label)
- Spawns implementation in fresh wrapix container (profile from bead label or flag)
- Waits for completion
- On failure (no RALPH_COMPLETE), retries with error context (see Retry below)
- Repeats until all issues complete
- Handles discovered work via `bd mol bond`
- **Does NOT `git push`** — commits land per-bead during the loop but the push is deferred to `ralph check` (see Push Gate below)
- **At molecule completion, auto-invokes `ralph check`** so the user isn't required to chain commands manually. The exec replaces the current process (`ralph run` exits through `ralph check`'s exit code)

**Push Gate:**
- `ralph run` never pushes; `ralph check` owns the push
- After `ralph check` emits `RALPH_COMPLETE` with no new beads created, check runs `git push` + `beads-push` against the current branch and its configured upstream (equivalent to `git push` with no args)
- If `ralph check` creates fix-up beads without a `ralph:clarify`, it auto-invokes `ralph run` on the new beads and then re-runs itself — the loop iterates until clean RALPH_COMPLETE, a clarify blocks it, or the iteration cap trips (see Iteration Cap)
- If `ralph check` produces a `ralph:clarify`, the workflow stops and waits for the user's `ralph msg` reply; no push

**Push failure handling:**
- **Non-fast-forward / rejected** — exit non-zero, print `pull/rebase then re-run ralph check`, leave molecule state unchanged. No automatic retry.
- **Detached HEAD** — refuse to push with a clear error. The user is expected to be on a branch for the full ralph workflow.
- **`beads-push` fails after `git push` succeeds** — exit non-zero with `run beads-push manually` hint. Code commits are on the remote; beads remain local and user-recoverable.

**Iteration Cap:**
- `loop.max-iterations = 3` in `config.nix` (per molecule, default 3) bounds the `run ↔ check` auto-iteration
- Each `ralph check` invocation that creates fix-up beads without a clarify increments the iteration counter
- On the 3rd unsuccessful iteration, check labels the most recent fix-up beads with `ralph:clarify` (description notes the iteration cap was hit) and stops — the user picks up via `ralph msg` as usual
- Iteration counter is persisted in `state/<label>.json` and reset on clean RALPH_COMPLETE (push) or when the user clears the clarify via `ralph msg`

**Retry with context:**
When a worker fails, retry automatically with the previous error output:
- `loop.max-retries = 2` in `config.nix` (per bead, default 2)
- On failure, the error output from the previous attempt is injected into the next attempt's `run.md` context as `PREVIOUS_FAILURE` template variable
- After max retries, the bead gets `ralph:clarify` label with failure details, loop moves on

**Single-issue mode (`--once` / `-1`)** — For debugging or manual control:
- Selects next ready issue from molecule
- Reads `profile:X` label from bead to determine container profile (fallback: base)
- Launches wrapix container with selected profile
- Loads `run.md` template with issue context
- Implements in fresh Claude session
- Updates issue status on completion
- Exits after one issue — does NOT auto-invoke `ralph check` (that's continuous-mode behavior only)

### `ralph status`

```bash
ralph status                # Show status for current spec (from state/current)
ralph status --spec <name>  # Show status for named spec
ralph status -s <name>      # Short form
ralph status --all          # Summary of all active workflows
```

Shows molecule progress for the resolved spec:
```
Ralph Status: my-feature
===============================
Molecule: bd-xyz123
Spec: specs/my-feature.md

Progress:
  [####------] 40% (4/10)

Current Position:
  [done]    Setup project structure
  [done]    Implement core feature
  [current] Write tests         <- you are here
  [ready]   Update documentation
  [blocked] Final review (waiting on tests)
```

**`--all` mode** shows a summary of all active workflows:
```
Active Workflows:
  my-feature      running  [####------] 40% (4/10)
  auth-refactor   todo     [----------]  0% (0/5)
  bugfix-123      done     [##########] 100% (3/3)
```

### `ralph logs`

```bash
ralph logs              # Find most recent error for current spec
ralph logs -n 50        # Show 50 lines of context before error
ralph logs --all        # Show full log without error filtering
ralph logs --spec <name>  # Show logs for named spec
ralph logs -s <name>      # Short form
```

Error-focused output: Scans for error patterns (exit code != 0, "error:", "failed"), shows context leading up to first match.

### `ralph check`

```bash
ralph check                     # Post-loop review of the active spec (default)
ralph check --spec <name>       # Post-loop review of a named spec
ralph check -s <name>           # Short form
ralph check -t                  # Validate templates (no Claude invocation; mutually exclusive with review)
```

`ralph check` has two jobs:
1. **Post-loop review** (default) — the canonical review of completed implementation work; also auto-invoked by `ralph run` at molecule completion and owns the push gate (see below)
2. **Template validation (`-t` / `--templates`)** — a static check runnable anywhere, also wired into `nix flake check`

**Template validation (`-t` / `--templates`):**
- Partial files exist
- Body files parse correctly
- No syntax errors in Nix expressions
- Dry-run render with dummy values to catch placeholder typos
- Also runs as part of `nix flake check`

**Post-loop review (default):**

An independent reviewer agent assesses the full deliverable for the resolved spec (via `--spec` or `state/current`). Runs inside a wrapix container with base profile. Context-pinning and companions are injected via standard partials, plus the compaction re-pin hook (see Compaction Re-Pin). Reviewer has full codebase access and reads implementation code, tests, `CLAUDE.md`, and related specs on demand.

Input context (in prompt):
- Spec file (`specs/<label>.md`)
- Beads summary (titles and status only — reviewer reads full descriptions on demand)
- `base_commit` SHA (reviewer runs `git diff` / `git log` as needed)
- Molecule ID

Reviewer responsibilities:
- Assess spec compliance, code quality, test adequacy, coherence
- Detect **invariant clashes** between the implementation and existing design invariants (architectural decisions, data-structure choices, documented constraints, non-functional requirements, out-of-scope items). Detection uses LLM judgment biased toward asking — when uncertain, ask
- Propose *contextual* options for each clash (not a fixed A/B/C menu), guided by the three-paths principle:
  1. **Preserve the invariant** — revert or rework the clashing change so the invariant holds
  2. **Keep the change on top of the invariant** inelegantly/inefficiently, with the debt recorded in spec/notes
  3. **Change the invariant** — update the spec to accommodate the change, then create follow-up tasks to realign code
- Present options with enough context for the user to pick — typically 2–4 options per clash, each naming the cost
- Create follow-up beads via `bd create` + `bd mol bond` for actionable fixes that don't need human judgment
- Flag invariant clashes with `ralph:clarify` + a bead whose description contains the proposed options; user answers via `ralph msg` with a free-form choice
- Emit RALPH_COMPLETE when the review pass is finished

**Auto-chain and push gate** (host-side control flow in `check.sh`):

1. Capture molecule bead count before invoking the reviewer
2. Run reviewer in container; wait for RALPH_COMPLETE (container runs `bd dolt push` on the way out — see Container bead sync protocol)
3. Host runs `bd dolt pull`, then re-counts beads + checks for any bead carrying `ralph:clarify`
4. Branch:
   - **Clean** (no new beads, no `ralph:clarify`) → `git push` + `beads-push` → exit 0
   - **Fix-up beads, no clarify, under cap** → increment `iteration_count` in state JSON, `exec ralph run` (auto-iterate — `ralph run` will re-invoke check on molecule completion)
   - **Fix-up beads, no clarify, at cap** → label the newest fix-up beads with `ralph:clarify` (description notes the cap was hit), print summary + `ralph msg` pointer, exit 0. No push
   - **`ralph:clarify` present** → print a summary of outstanding questions + pointer to `ralph msg`, exit 0. No push. User responds via `ralph msg`; next `ralph run` invocation resumes the loop
5. Review failures (no RALPH_COMPLETE, or Claude error) exit non-zero without pushing

Template: `check.md` in `lib/ralph/template/`. Reuses partials: `context-pinning`, `spec-header`, `companions-context`, `exit-signals`. Variables: `BEADS_SUMMARY`, `BASE_COMMIT`, `MOLECULE_ID`.

Exit signals: RALPH_COMPLETE, RALPH_BLOCKED, RALPH_CLARIFY.

Exit codes: 0 = pass (pushed) or clarify-pending (awaiting user); 1 = review failure or template validation error.

### `ralph msg`

Human interface for responding to agent questions tagged with `ralph:clarify`.

```bash
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
- List mode shows a table: ID, spec, source, question
- Reply stores the answer in bead notes and removes `ralph:clarify` label — the run loop's next iteration picks it up
- On successful reply, prints a one-line resume hint: `Clarify cleared on <id>. Resume with: ralph run` (or `ralph run -s <label>` when the resolved spec differs from `state/current`). No automatic resume — the user just decided the answer and is best placed to decide when to relaunch the loop
- Dismiss removes `ralph:clarify` label with a note that the agent should work around it
- Abstracts bead storage — today uses bead labels/notes, interface is ralph-level

### `ralph tune`

**Interactive mode** (no stdin):
```bash
ralph tune
> What would you like to change?
> "Add guidance about handling blocked beads"
>
> Analyzing templates...
> This should go in run.md, section "Instructions"
>
> [makes edit to .wrapix/ralph/template/run.md]
> [runs ralph check]
> ✓ Template valid
```

**Integration mode** (stdin with diff):
```bash
ralph sync --diff | ralph tune
> Analyzing diff...
>
> Change 1/2: run.md lines 35-40
> + 6. **Blocked vs Waiting**: ...
>
> Where should this go?
>   1. Keep in run.md
>   2. Move to partial
>   3. Create new partial
> > 1
>
> Accept this change? [Y/n] y
> ✓ Change applied
```

AI-driven interview that asks questions until user accepts or abandons.

### `ralph sync`

```bash
ralph sync           # Update local templates from packaged
ralph sync --diff    # Show local template changes vs packaged (preview)
ralph sync --dry-run # Preview sync without executing
```

Synchronizes local templates with packaged versions:

1. Creates `.wrapix/ralph/template/` with fresh packaged templates
2. Moves existing customized templates to `.wrapix/ralph/backup/`
3. Copies all templates including variants and `partial/` directory
4. Scaffolds project documentation (skip-if-exists):
   - `docs/README.md` — spec index stub with Specs/Beads/Purpose table
   - `docs/architecture.md` — architecture overview stub
   - `docs/style-guidelines.md` — code style guidelines stub
   - `AGENTS.md` — agent instructions pointing at the docs above

**`--diff` mode**: Shows changes between local templates and packaged versions. Pipe to `ralph tune` for integration:
```bash
ralph sync --diff | ralph tune
```

**Directory structure after sync:**
```
.wrapix/ralph/
├── config.nix
├── template/            # Fresh from packaged
│   ├── partial/
│   │   ├── companions-context.md
│   │   ├── context-pinning.md
│   │   ├── exit-signals.md
│   │   ├── interview-modes.md
│   │   └── spec-header.md
│   ├── check.md
│   ├── plan-new.md
│   ├── plan-update.md
│   ├── run.md
│   ├── todo-new.md
│   └── todo-update.md
└── backup/              # User customizations (if any)
    └── ...
```

Use `ralph sync --diff` to see what changed, then `ralph tune` to merge customizations from backup.

### `ralph use`

```bash
ralph use <name>        # Switch active workflow
```

Sets `state/current` to the given label after validation:
1. Validates the spec exists (`specs/<name>.md` or hidden spec in `state/`)
2. Validates `state/<name>.json` exists (workflow must be initialized via `ralph plan`)
3. Writes the label to `state/current`
4. Errors with clear message if either validation fails

## `ralph:clarify` Label

Beads waiting on human response are tagged with `ralph:clarify`. Used by implementation workers and the reviewer agent. The run loop filters out beads with this label when selecting the next bead to work. Each iteration re-queries, so when a human removes the label via `ralph msg`, the bead becomes eligible on the next pass. A notification is emitted when the label is first applied.

## Workflow Phases

```
plan → todo → run → check → (done + push)
  │       │     │      │        │
  │       │     │      │        └─ git push + beads-push (only on RALPH_COMPLETE + no new beads)
  │       │     │      ├─ Invariant-clash detection → ralph:clarify (stop, wait for ralph msg)
  │       │     │      ├─ Fix-up beads found → exec ralph run (auto-iterate)
  │       │     │      └─ Reviewer reads code + spec; bonds fix-ups to molecule
  │       │     ├─ Implementation + bd mol bond (discovered work); commits per-bead, no push
  │       │     └─ At molecule completion → exec ralph check
  │       └─ Molecule creation from specs/<label>.md
  └─ Spec interview → writes specs/<label>.md
     ├─ "one by one" → guided walk through open questions with defaults
     └─ "polish the spec" → end-of-session read-through for consistency / ambiguity

Update cycle (for existing specs):
plan --update → todo → run → check → (done + push)
      │            │
      │            ├─ git diff base_commit..HEAD -- spec (tier 1)
      │            ├─ OR compare spec vs existing tasks (tier 2)
      │            ├─ Create tasks ONLY for new/changed requirements
      │            └─ Store HEAD as base_commit on success
      └─ LLM edits spec directly → commits changes
         ├─ Detects invariant clashes → asks user (no RALPH_CLARIFY needed, interactive)
         ├─ "one by one" / "polish the spec" modes available
         └─ Stores transient hints in state/<label>.json implementation_notes

Auto-iteration loop (run ↔ check), bounded by loop.max-iterations:
run → check → (new beads?) ─┬─ yes + no clarify + under cap → exec ralph run → check → …
                            ├─ yes + no clarify + at cap    → set ralph:clarify (escalate) → stop
                            ├─ yes + clarify                → stop, ralph msg → ralph run → check → …
                            └─ no                           → git push + beads-push → done
```

## Container Execution

Ralph runs Claude-calling commands inside wrapix containers for isolation and reproducibility.

| Command | Execution | Profile |
|---------|-----------|---------|
| `ralph plan` | wrapix container | base |
| `ralph todo` | wrapix container | base |
| `ralph run` | host (orchestrator) | N/A (spawns containerized work per-issue) |
| `ralph run --once` | wrapix container | from bead label or `--profile` flag (fallback: base) |
| `ralph status` | host | N/A (utility) |
| `ralph logs` | host | N/A (utility) |
| `ralph check` (default) | wrapix container | base |
| `ralph check -t` | host | N/A (utility) |
| `ralph msg` | host | N/A (utility) |
| `ralph tune` | host | N/A (utility) |
| `ralph sync` | host | N/A (utility) |
| `ralph use` | host | N/A (utility) |

**Rationale:**
- `plan` and `todo` involve AI decision-making that benefits from isolation
- `run --once` performs implementation work requiring language toolchains
- `run` (continuous) is a simple orchestrator that spawns containerized steps
- Utility commands don't invoke Claude and run directly on host

**Container bead sync protocol:** All container-executed commands (`plan`, `todo`, `run --once`, `check`) follow this exit sequence:
1. Command logic completes, outputs `RALPH_COMPLETE`
2. Container-side `<cmd>.sh` runs `bd dolt push` (syncs container `.beads/` → Dolt remote)
3. Container exits
4. Host-side `<cmd>.sh` runs `bd dolt pull` (syncs Dolt remote → host `.beads/`)

This is necessary because the container has its own `.beads/` database (not bind-mounted). Without the push/pull handoff, beads created inside the container are lost when the container exits. The host-side pull is the final step; if `bd dolt push` failed inside the container, the pull gets stale data and the host emits an informational warning with recovery hints.

## Compaction Re-Pin

Claude Code auto-compacts long-running sessions when the context window fills. The initial rendered template content (label, spec path, companion manifest list, exit-signal instructions, issue details) can be pushed out of the compacted transcript, causing the model to drift — forgetting which spec it's working on, which exit signals to use, which companion files exist to consult.

Ralph configures a `SessionStart` hook with matcher `"compact"` so a condensed re-pin is re-injected into the session on the next model turn. The re-pin deliberately excludes the full spec body (the model can re-read `specs/<label>.md` on demand); keeping the injection small protects the savings from compaction.

### Hook Scope

The hook is registered for every container-executed Claude session. Host-side commands (`status`, `logs`, `check -t`, `msg`, `tune`, `sync`, `use`) do not invoke Claude and do not register the hook.

| Command | Re-pin content |
|---------|----------------|
| `ralph plan` (new/update) | Label, spec path, mode (`new`/`update`), exit signals |
| `ralph todo` | Label, spec path, molecule ID (if set), companion paths, exit signals |
| `ralph run --once` | Label, spec path, issue ID, title, companion paths, exit signals |
| `ralph check` | Label, spec path, molecule ID, base commit, exit signals |

The re-pin does NOT include the full spec body, companion manifest bodies, full issue description, or task list — these can be re-read on demand from `specs/<label>.md`, the companion directories, `bd show <id>`, and `bd mol current`.

### Implementation

Per container-executed command, the command's shell script (e.g., `plan.sh`, `todo.sh`, `run.sh`, `check.sh`) writes two files under a per-label runtime directory before invoking wrapix:

```
.wrapix/ralph/runtime/<label>/
├── repin.sh              # Emits the condensed re-pin as JSON on stdout
└── claude-settings.json  # SessionStart hook fragment pointing at repin.sh
```

The hook fragment uses the `hookSpecificOutput.additionalContext` form so output is explicitly treated as injected context (plain stdout also works; JSON is preferred for clarity and the 10KB cap is well within ralph's needs):

```json
{
  "hookSpecificOutput": {
    "hookEventName": "SessionStart",
    "additionalContext": "Label: my-feature\nSpec: specs/my-feature.md\n..."
  }
}
```

The settings fragment shape:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "compact",
        "hooks": [
          { "type": "command", "command": "/workspace/.wrapix/ralph/runtime/<label>/repin.sh" }
        ]
      }
    ]
  }
}
```

### Settings Merge

`lib/sandbox/linux/entrypoint.sh` gains a merge step after the existing `/workspace/.claude/settings.json` block: if `/workspace/.wrapix/ralph/runtime/<label>/claude-settings.json` exists (path resolved via a `RALPH_RUNTIME_DIR` env var exported by ralph on the host), its `hooks` tree is deep-merged into `~/.claude/settings.json`. This keeps ralph's per-invocation hook config separate from user customizations in `settings.local.json` and from the sandbox-wide `baseClaudeSettings.hooks` defined in `lib/sandbox/default.nix`.

The merge concatenates arrays under each hook event rather than replacing them, so ralph's `SessionStart[compact]` coexists with the existing `Notification` hook and any user-configured hooks.

### Lifecycle

1. Host-side shell computes the re-pin content from the command's variables and writes both files under `.wrapix/ralph/runtime/<label>/`.
2. Host invokes wrapix with `--env RALPH_RUNTIME_DIR=.wrapix/ralph/runtime/<label>` (wrapix scrubs host env by default; explicit `--env` is required). The runtime directory itself reaches the container through the existing `/workspace` bind mount — no extra mount is needed, but this dependency on the bind mount is a precondition of the feature.
3. Container entrypoint reads the env var, merges the settings fragment into `~/.claude/settings.json`, and starts Claude.
4. On auto-compaction, the `SessionStart` hook fires and the re-pin re-enters context.
5. When the container exits, the host-side shell removes `.wrapix/ralph/runtime/<label>/`.

The runtime directory is not committed to git; `.wrapix/ralph/runtime/` is added to `.gitignore`.

### Helpers

`lib/ralph/cmd/util.sh` gains two helpers:

- `build_repin_content <label> <command> [key=value ...]` — composes the condensed re-pin string from known keys (spec path, molecule ID, issue ID, companion paths, exit signals for the command).
- `install_repin_hook <label> <content>` — writes `repin.sh` (chmod +x) and `claude-settings.json` into `.wrapix/ralph/runtime/<label>/` and exports `RALPH_RUNTIME_DIR`. Called by each container-executed command before `run_claude_stream`.

Cleanup of the runtime directory is handled by a trap in the command script so it runs on both success and failure paths.

## Profile Selection

Profiles determine which language toolchains are available in the wrapix container.

### Available Profiles

| Profile | Includes |
|---------|----------|
| `base` | Core tools, git, standard utilities |
| `rust` | base + Rust toolchain, cargo |
| `python` | base + Python, pip, venv |
| `debug` | base + debugging tools (see tmux-mcp spec) |

### Profile Assignment Flow

1. **`ralph todo`** — LLM analyzes each task and assigns `profile:X` label based on:
   - Files the task will touch (`.rs` → rust, `.py` → python, `.nix` → base)
   - Tools required (cargo, pytest, etc.)
   - Task description context

2. **Task creation** includes profile label:
   ```bash
   bd create --title="Implement parser" --labels "spec:my-feature,profile:rust" ...
   bd create --title="Update docs" --labels "spec:my-feature,profile:base" ...
   ```

3. **`ralph run`** reads profile from bead:
   ```bash
   # Get profile label from bead
   profile=$(bd show "$issue_id" --json | jq -r '.labels[] | select(startswith("profile:")) | split(":")[1]')
   profile="${profile:-base}"
   ```

4. **Override** via `--profile` flag takes precedence:
   ```bash
   ralph run --once --profile=rust  # Ignore bead label, use rust
   ```

### Per-Task Profiles

Different tasks in the same molecule may have different profiles. The LLM decides per-task based on what that specific task needs:

| Task | Profile |
|------|---------|
| "Implement Rust parser" | `profile:rust` |
| "Write Python test harness" | `profile:python` |
| "Update Nix build config" | `profile:base` |
| "Add documentation" | `profile:base` |

This is more accurate than spec-level detection because tasks often span multiple languages.

## Template System

### Nix-Native Templates

Templates are defined as Nix expressions with static validation:

```nix
# lib/ralph/template/default.nix
{ lib }:
let
  mkTemplate = { body, partials ? [], variables }:
    let
      resolvedPartials = map (p: builtins.readFile p) partials;
      content = builtins.readFile body;
    in {
      inherit content variables partials;
      render = vars:
        assert lib.assertMsg
          (builtins.all (v: vars ? ${v}) variables)
          "Missing required variables: ${builtins.toJSON variables}";
        lib.replaceStrings
          (map (v: "{{${v}}}") variables)
          (map (v: vars.${v}) variables)
          content;
    };
in {
  plan-new = mkTemplate {
    body = ./plan-new.md;
    partials = [ ./partial/context-pinning.md ./partial/exit-signals.md ];
    variables = [ "PINNED_CONTEXT" "LABEL" "SPEC_PATH" ];
  };
  # ... other templates
}
```

### Partials

Shared content via `{{> partial-name}}` markers:

```markdown
## Instructions

{{> context-pinning}}

1. Read the spec...
```

Resolved during template rendering.

### Template Structure

```
lib/ralph/template/
├── default.nix              # Template definitions + validation
├── partial/
│   ├── companions-context.md # Companion manifest injection
│   ├── context-pinning.md   # Project context loading
│   ├── exit-signals.md      # Exit signal format
│   ├── interview-modes.md   # "one by one" / "polish the spec" fast phrases
│   └── spec-header.md       # Label, spec path block
├── check.md                 # Post-loop reviewer prompt (invariant-clash aware)
├── plan-new.md              # New spec interview
├── plan-update.md           # Update existing spec
├── todo-new.md              # Create molecule
├── todo-update.md           # Bond new tasks
└── run.md                   # Single-issue implementation
```

### Template Variables

| Variable | Source | Used By |
|----------|--------|---------|
| `PINNED_CONTEXT` | Read from `pinnedContext` file | all |
| `LABEL` | From command args | all |
| `SPEC_PATH` | Computed from label + mode | all |
| `SPEC_CONTENT` | Read from spec file | todo-new, run |
| `EXISTING_SPEC` | Read from `specs/<label>.md` | plan-update, todo-update |
| `SPEC_DIFF` | From `git diff base_commit..HEAD` (tier 1) | todo-update |
| `EXISTING_TASKS` | From molecule task list (tier 2) | todo-update |
| `COMPANIONS` | From `read_manifests` (companion directories) | plan-update, todo-new, todo-update, run, check |
| `IMPLEMENTATION_NOTES` | From `implementation_notes` in `state/<label>.json` | todo-new, todo-update |
| `MOLECULE_ID` | From `state/<label>.json` | todo-update, run |
| `ISSUE_ID` | From `bd ready` | run |
| `TITLE` | From issue | run |
| `DESCRIPTION` | From issue | run |
| `BEADS_SUMMARY` | From molecule (titles + status) | check |
| `BASE_COMMIT` | From `state/<label>.json` | check |
| `PREVIOUS_FAILURE` | From previous attempt error output | run (retry only) |
| `EXIT_SIGNALS` | Template-specific list | all (via partial) |

### Flake Check Integration

```nix
# flake.nix
{
  checks.${system} = {
    ralph-templates = ralph.lib.validateTemplates {
      templates = ./lib/ralph/template;
    };
  };
}
```

## Project Configuration

Projects configure ralph via `.wrapix/ralph/config.nix` (local project overrides):

```nix
# .wrapix/ralph/config.nix
{
  # Wrapix flake reference (provides container profiles)
  wrapix = "github:user/wrapix";  # or local path

  # Context pinning - file read for {{PINNED_CONTEXT}}
  pinnedContext = "docs/README.md";

  # Spec locations
  specDir = ./specs;
  stateDir = ./state;

  # Template overlay (optional, for local customizations)
  templateDir = ./.wrapix/ralph/template;  # null = use packaged only

  # Run loop settings
  loop = {
    max-retries = 2;      # per bead, retry with PREVIOUS_FAILURE context
    max-iterations = 3;   # per molecule, bounds run ↔ check auto-iteration before escalating via ralph:clarify
  };
}
```

**Defaults** (when no config exists):
```nix
{
  wrapix = null;            # Error if container commands need it
  pinnedContext = "docs/README.md";
  specDir = ./specs;
  stateDir = ./state;
  templateDir = null;       # Use packaged templates only
}
```

**Template loading order:**
1. Check `templateDir` (local overlay) first
2. Fall back to packaged templates

**Profile resolution:**
- Profiles (base, rust, python, debug) are defined in wrapix (see profiles.md spec)
- Ralph references profiles by name; wrapix provides the actual container configuration

## Template Content Requirements

### Partials

**`partial/context-pinning.md`:**
```markdown
## Context Pinning

First, read the project overview to understand project terminology:

{{PINNED_CONTEXT}}
```

**`partial/exit-signals.md`:**
```markdown
## Exit Signals

Output ONE of these at the end of your response:

{{EXIT_SIGNALS}}
```

**`partial/companions-context.md`:**
```markdown
{{COMPANIONS}}
```

Ships with just `{{COMPANIONS}}`; overridable by local template overlay.

**`partial/spec-header.md`:**
```markdown
## Current Feature

Label: {{LABEL}}
Spec file: {{SPEC_PATH}}
```

**`partial/interview-modes.md`:**
```markdown
## Interview Modes

The user may request one of these structured sub-modes at any point in the interview. Phrase matching is loose — respond to intent, not exact wording.

- **"one by one"** (also: "let's go through one by one", "go through them one at a time", etc.) — When you have multiple open design questions, present them individually in sequence. For each question, propose a suggested default with a short rationale, and wait for the user to accept, reject, or adjust before moving to the next. Optimizes for user attention: small decisions per turn, defaults ready to rubber-stamp.

- **"polish the spec"** (also: "polish this spec", "give it a polish", "do a polish pass", etc.) — Read the full spec end-to-end and report on: readability issues (unclear phrasing, missing context), consistency issues (contradictions between sections, terminology drift), ambiguities (statements that could be read multiple ways), and structural problems (misplaced content, missing sections). Propose specific edits for each finding. Typically run at end of a planning session, but available at any point.

Both modes remain within planning-only scope — no code changes, only spec edits.
```

Ships with the above content; overridable by local template overlay.

### plan-new.md

**Purpose:** Conduct spec interview for new features

**Required sections:**
1. Role statement — "You are conducting a specification interview"
2. Planning-only warning — No code, only spec output
3. `{{> context-pinning}}`
4. `{{> spec-header}}`
5. Interview guidelines — One question at a time, capture terminology, identify code locations, clarify scope, define success criteria
6. Interview flow — Describe idea → clarify → write spec → RALPH_COMPLETE
7. Spec file format — Title, problem, requirements, affected files, success criteria, out of scope
8. Implementation notes section — Optional transient context, stripped on finalize
9. `{{> interview-modes}}` — Documents the "one by one" and "polish the spec" fast phrases (see partial below). Phrase matching is loose
10. `{{> exit-signals}}`

**Exit signals:** RALPH_COMPLETE, RALPH_BLOCKED, RALPH_CLARIFY

### plan-update.md

**Purpose:** Gather additional requirements for existing specs

**Required sections:**
1. Role statement — "You are refining an existing specification"
2. Planning-only warning — No code
3. `{{> context-pinning}}`
4. `{{> spec-header}}`
5. Existing spec display — Show current spec from `specs/<label>.md` for reference
6. `{{> companions-context}}` — after existing spec, before update guidelines
7. Update guidelines — Discuss NEW requirements only
8. Invariant-clash awareness — Before committing a spec change, scan the existing spec for invariants the change may clash with (architectural decisions, data-structure choices, explicit constraints, non-functional requirements, out-of-scope items). When a potential clash is found, pause the interview and ask the user to pick a path, proposing *contextual* options for the specific clash — guided by the three-paths principle (preserve invariant / keep on top inelegantly / change invariant) but not limited to exactly three or those exact framings. Bias toward asking when uncertain — the cost of asking is low compared to a silent wrong choice
9. Implementation notes guidance — Store transient implementation hints in state file `implementation_notes` array, not in the spec
10. Instructions — LLM edits `specs/<label>.md` directly during the interview; commits changes at end of session (git-tracked specs only; hidden specs just save file)
11. `{{> interview-modes}}` — Documents the "one by one" and "polish the spec" fast phrases
12. `{{> exit-signals}}`

**Exit signals:** RALPH_COMPLETE, RALPH_BLOCKED, RALPH_CLARIFY

### todo-new.md

**Purpose:** Convert spec to molecule with tasks

**Required sections:**
1. Role statement — "You are decomposing a specification into tasks"
2. `{{> context-pinning}}`
3. `{{> spec-header}}`
4. Spec content — Full spec to decompose
5. `{{> companions-context}}` — after spec content, before task breakdown guidelines
6. Task breakdown guidelines — Self-contained, ordered by deps, one objective per task
7. Profile assignment guidance — Assign `profile:X` per-task based on implementation needs:
   - Tasks touching `.rs` files or using cargo → `profile:rust`
   - Tasks touching `.py` files or using pytest/pip → `profile:python`
   - Tasks touching only `.nix`, `.sh`, `.md` files → `profile:base`
8. Molecule creation — Create epic, child tasks with profile labels, set dependencies
9. README update — Write molecule ID to the Beads column of the pinned-context file (`docs/README.md` by default) (required for cross-machine state recovery)
10. `{{> exit-signals}}`

**Exit signals:** RALPH_COMPLETE, RALPH_BLOCKED

### todo-update.md

**Purpose:** Add new tasks to existing molecule

**Required sections:**
1. Role statement — "You are adding tasks to an existing molecule"
2. `{{> context-pinning}}`
3. `{{> spec-header}}`
4. Existing spec — Show `specs/<label>.md` for context (what's already implemented)
5. `{{> companions-context}}` — after existing spec, before diff/tasks section
6. Spec diff or existing tasks — Receives both `{{SPEC_DIFF}}` and `{{EXISTING_TASKS}}`; one is always empty depending on tier. Instructions: "if SPEC_DIFF is provided, use that to identify new requirements; otherwise use EXISTING_TASKS to compare against the spec and identify gaps."
7. Profile assignment guidance — Assign `profile:X` per-task based on implementation needs
8. Task creation — Create tasks ONLY for new/changed requirements, bond to molecule
9. README backfill — If the pinned-context file (`docs/README.md` by default) Beads column is empty for this spec, fill in the molecule ID
10. `{{> exit-signals}}`

**`EXISTING_TASKS` format** (tier 2/3):
```markdown
### wx-abc1: Implement parser (done)
Parse input files using the new grammar...

### wx-abc2: Write tests (in_progress)
Add unit tests for parser edge cases...
```

**Key behavior:** Only create tasks for changes identified via diff or gaps identified against existing tasks.

**Exit signals:** RALPH_COMPLETE, RALPH_BLOCKED

### run.md

**Purpose:** Implement single issue in fresh context

**Required sections:**
1. `{{> context-pinning}}`
2. `{{> spec-header}}`
3. `{{> companions-context}}` — after spec header, before issue details
4. Issue details — ID, title, description
5. Instructions:
   1. **Understand** — Read spec and issue before changes
   2. **Test strategy** — Property-based vs unit tests
   3. **Implement** — Write code following spec
   4. **Discovered work** — Create issue, bond to molecule (sequential vs parallel)
   5. **Quality gates** — Tests pass, lint passes, changes committed (commit yes; **do not push** — push is owned by `ralph check`)
   6. **Blocked vs waiting** — Distinguish dependency blocks from true blocks:
      - Need user input? → `RALPH_BLOCKED: <reason>`
      - Need other beads done? → Add dep with `bd dep add`, output `RALPH_COMPLETE`
   7. **Already implemented** — If the task's work is already done in the codebase, verify correctness, close the issue, and move on
6. `{{> exit-signals}}`

**Exit signals:** RALPH_COMPLETE, RALPH_BLOCKED, RALPH_CLARIFY

### check.md

**Purpose:** Post-loop review of completed implementation work, guarded by the push gate.

**Required sections:**
1. Role statement — "You are an independent reviewer assessing the completed deliverable for spec compliance, code quality, test adequacy, and coherence with existing invariants"
2. `{{> context-pinning}}`
3. `{{> spec-header}}`
4. `{{> companions-context}}`
5. Beads summary — `{{BEADS_SUMMARY}}` (titles + status only; reviewer reads descriptions via `bd show` on demand)
6. Base commit — `{{BASE_COMMIT}}` (reviewer runs `git diff` / `git log` as needed)
7. Molecule ID — `{{MOLECULE_ID}}`
8. Review dimensions — spec compliance, code quality, test adequacy, coherence, invariant clashes
9. **Invariant-clash detection and the three-paths principle** — Must be a first-class section of the template, including:
   - Definition of invariant (architectural decisions, data-structure choices, documented constraints, non-functional requirements, out-of-scope items)
   - Detection posture: LLM judgment biased toward asking — when uncertain, ask
   - Three-paths principle as *guidance* (not a fixed menu): preserve invariant / keep on top inelegantly / change invariant
   - Instruction: propose *contextual* options tailored to the specific clash, typically 2–4 options per clash, each naming the cost. Do NOT emit a fixed A/B/C menu
   - Handling: for each clash, create a bead whose description contains the proposed options, add the `ralph:clarify` label, bond to molecule
10. Fix-up bead creation — for issues that don't require human judgment, create beads via `bd create` + `bd mol bond` with appropriate `profile:X` labels
11. `{{> exit-signals}}`

**Variables:** `BEADS_SUMMARY`, `BASE_COMMIT`, `MOLECULE_ID`

**Exit signals:** RALPH_COMPLETE, RALPH_BLOCKED, RALPH_CLARIFY

## State Management

### Per-Label State Files

Each workflow has its own state file at `state/<label>.json`:

**`state/<label>.json`:**
```json
{
  "label": "my-feature",
  "spec_path": "specs/my-feature.md",
  "molecule": "wx-9mvh",
  "base_commit": "abc123def456...",
  "companions": ["specs/e2e", "docs/api"]
}
```

| Field | Description |
|-------|-------------|
| `label` | Feature identifier (required) |
| `spec_path` | Full path to spec file (required) |
| `molecule` | Beads molecule ID (set by `ralph todo`) |
| `base_commit` | Git commit SHA at which spec was last fully tasked (set by `ralph todo` on success) |
| `companions` | Array of repo-relative directory paths containing `manifest.md` files |
| `implementation_notes` | Array of strings — transient implementation hints for task creation (optional; cleared by `ralph todo` on `RALPH_COMPLETE`) |
| `iteration_count` | Integer — current position in the `run ↔ check` auto-iteration (optional; reset on clean RALPH_COMPLETE push or clarify clear via `ralph msg`) |

### Active Workflow Pointer

**`state/current`** (plain text, no extension) holds the active label name:
```
my-feature
```

This file is the default when no `--spec` flag is given. Set by `ralph plan` and `ralph use`.

### Spec Label Resolution

Commands that accept `--spec/-s` resolve the target workflow as follows:
1. If `--spec <name>` provided → use `state/<name>.json`
2. If no `--spec` → read label from `state/current` → use `state/<label>.json`
3. If `state/current` does not exist and no `--spec` given → error with clear message

Commands with `--spec` support: `ralph todo`, `ralph run`, `ralph status`, `ralph logs`, `ralph check`
Commands without `--spec`: `ralph plan` (takes label as positional arg), `ralph tune`, `ralph sync`, `ralph use` (takes label as positional arg)

### Backwards Compatibility

Serial workflow is unchanged: `ralph plan` sets `state/current`, subsequent commands pick it up automatically. The only structural difference is the state file location (`state/<label>.json` + `state/current` vs the old singleton `state/current.json`).

Existing specs without `base_commit` are handled via four-tier fallback:
- If molecule in state JSON → tier 2 (LLM compares spec against tasks)
- If no state file but molecule in README → tier 3 (reconstruct state, then tier 2)
- If no molecule anywhere → tier 4 (treats as new, creates full molecule)

After first successful `ralph todo`, `base_commit` is set and fast-path diffing works going forward.

## Companion Content

Specs can declare companion directories whose manifest is injected into templates, giving the LLM awareness of related content it can read on demand.

- `state/<label>.json` gains an optional `companions` field: an array of directory paths relative to repo root
- Each companion directory must exist and contain a `manifest.md` — error if directory does not exist, separate error if directory exists but `manifest.md` is missing
- `read_manifests` in `util.sh` reads only `manifest.md` from each directory, wrapping each in XML-style tags:

  ```xml
  <companion path="specs/e2e">
  (contents of specs/e2e/manifest.md)
  </companion>
  ```

- Individual companion files are never bulk-injected; the LLM reads them on demand based on manifest guidance
- No exclusion patterns, no individual file entries, no size limits
- Any repo-relative path is valid (not restricted to `specs/`)
- No CLI flags for managing companions — users edit `state/<label>.json` manually
- Companions are not included in `plan-new` template (state JSON doesn't exist yet)

## Implementation Notes

State files can contain an optional `implementation_notes` field: an array of strings providing transient implementation hints gathered during `ralph plan -u` spec update interviews. These are details that help the implementer but don't belong in the permanent spec (e.g., "remove the rustup bootstrap block", "use fenix's fromToolchainFile").

- Populated during `ralph plan -u` interviews — the interviewer stores notes in the state file rather than adding an "Implementation Notes" section to the spec markdown
- Read by `ralph todo` from `state/<label>.json` and formatted as a markdown bullet list
- Passed to both `todo-new.md` and `todo-update.md` templates as the `IMPLEMENTATION_NOTES` variable
- Provide additional context to the LLM during task creation without polluting the permanent spec
- **Lifecycle:** notes describe *how* to implement the current diff; once `ralph todo` has folded them into task descriptions they've served their purpose. `ralph todo` clears `implementation_notes` from `state/<label>.json` atomically with setting `base_commit` on `RALPH_COMPLETE` — the same success signal that advances the spec cursor. This prevents stale hints from silently biasing future task creation

The `strip_implementation_notes` function in `util.sh` remains as a backward-compatibility safety net for specs that manually include a `## Implementation Notes` section.

## Git-Based Spec Diffing

Replace the transient `state/<label>.md` intermediary with git-based diffing using a `base_commit` field in state JSON.

- `ralph plan -u` edits `specs/<label>.md` directly (LLM commits the changes for git-tracked specs)
- `ralph plan -u -h` edits the hidden spec directly but does NOT commit
- `ralph todo` uses four-tier detection (see `ralph todo` command section)
- `compute_spec_diff` helper in `util.sh` encapsulates the four-tier fallback
- `discover_molecule_from_readme` helper in `util.sh` parses the pinned-context file (`docs/README.md` by default) to find molecule IDs by spec label
- `--since <commit>` flag forces tier 1 with the given commit
- No explicit migration required — existing specs handled via tier 2/3/4 fallback

### Cross-Machine State Recovery (Tier 3)

When `state/<label>.json` does not exist (e.g., working from a different machine), `ralph todo`:

1. Parses the pinned-context file (`docs/README.md` by default) to find the row whose spec filename matches `<label>.md`
2. Extracts the Beads column value (the molecule ID) from that row
3. Validates the molecule exists via `bd show <id>` — if invalid/not found, falls through to tier 4
4. Reconstructs `state/<label>.json` with:
   - `label`: the spec label
   - `spec_path`: `specs/<label>.md`
   - `molecule`: the discovered molecule ID
   - `base_commit`: omitted (not recoverable)
   - `companions`: empty array (not recoverable; user can add manually)
5. Proceeds as tier 2 (molecule-based comparison against existing tasks)

**README parsing**: Simple grep/awk to find the row matching the spec filename and extract the Beads column. The table format is stable and controlled by the project.

## Spec File Format

```markdown
# Feature Name

Overview of the feature.

## Problem Statement

Why this feature is needed.

## Requirements

### Functional
1. Requirement one
2. Requirement two

### Non-Functional
1. Performance requirement

## Affected Files

| File | Role |
|------|------|
| `path/to/file.nix` | Description |

## Success Criteria

- [ ] Criterion one
- [ ] Criterion two

## Out of Scope

- Thing not included
```

## Affected Files

| File | Role |
|------|------|
| `lib/ralph/cmd/ralph.sh` | Main dispatcher |
| `lib/ralph/cmd/plan.sh` | Feature initialization + spec interview |
| `lib/ralph/cmd/todo.sh` | Issue creation (renamed from ready.sh) |
| `lib/ralph/cmd/run.sh` | Issue work (merged from step.sh + loop.sh), retry logic; commits per-bead but does NOT push; at molecule completion `exec ralph check` |
| `lib/ralph/cmd/status.sh` | Progress display |
| `lib/ralph/cmd/logs.sh` | Error-focused log viewer |
| `lib/ralph/cmd/check.sh` | Post-loop review (default) and template validation (`-t`); runs `bd dolt push` in container and `bd dolt pull` on host so fix-up/clarify beads created by the reviewer reach the host before re-counting; auto-iteration chain (`exec ralph run` on fix-up beads; `git push` + `beads-push` on clean RALPH_COMPLETE) |
| `lib/ralph/cmd/msg.sh` | Async human communication |
| `lib/ralph/cmd/tune.sh` | Template editing (interactive + integration) |
| `lib/ralph/cmd/sync.sh` | Template sync from packaged (includes --diff) |
| `lib/ralph/cmd/use.sh` | Active workflow switching with validation |
| `lib/ralph/cmd/util.sh` | Shared helper functions (includes `resolve_spec_label`, `read_manifests`, `compute_spec_diff`, `discover_molecule_from_readme`, `ralph:clarify` label management, `build_repin_content`, `install_repin_hook`) |
| `lib/ralph/template/` | Prompt templates |
| `lib/ralph/template/check.md` | Reviewer agent prompt |
| `lib/ralph/template/default.nix` | Nix template definitions (includes check template, `PREVIOUS_FAILURE` variable) |
| `lib/ralph/template/partial/companions-context.md` | Companion manifest injection partial |
| `lib/ralph/template/partial/interview-modes.md` | Documents "one by one" and "polish the spec" fast phrases |
| `lib/sandbox/linux/entrypoint.sh` | Merge ralph runtime `claude-settings.json` from `$RALPH_RUNTIME_DIR` into `~/.claude/settings.json` when the env var is set |
| `.gitignore` | Exclude `.wrapix/ralph/runtime/` |

## Integration with Beads Molecules

Ralph uses `bd mol` for work tracking:

- **Specs are NOT molecules** — Specs are persistent markdown; molecules are work batches
- **Each `ralph todo` creates/updates a molecule** — Epic becomes molecule root
- **Update mode bonds to existing molecules** — New tasks attach to prior work
- **Molecule ID stored in `state/<label>.json`** — Enables `ralph status` convenience wrapper

**Key molecule commands used by Ralph:**

| Command | Used by | Purpose |
|---------|---------|---------|
| `bd create --type=epic` | `ralph todo` | Create molecule root |
| `bd mol progress` | `ralph status` | Show completion % |
| `bd mol current` | `ralph status` | Show position in DAG |
| `bd mol bond` | `ralph run` | Attach discovered work |
| `bd mol stale` | `ralph status` | Warn about orphaned molecules |

**Not used by Ralph** (user calls directly):
- `bd mol squash` — User decides when to archive
- `bd mol burn` — User decides when to abandon

## Success Criteria

- [ ] `ralph plan -n <label>` creates new spec in `specs/`
  [verify](../tests/ralph/run-tests.sh#test_plan_flag_validation)
- [ ] `ralph plan -h <label>` creates new spec in `state/`
  [verify](../tests/ralph/run-tests.sh#test_plan_flag_validation)
- [ ] `ralph plan -u <label>` validates spec exists before updating
  [verify](../tests/ralph/run-tests.sh#test_plan_flag_validation)
- [ ] `ralph plan -u <label>` edits spec directly (no `state/<label>.md` created)
  [verify](../tests/ralph/run-tests.sh#test_plan_update_direct_edit)
- [ ] `ralph plan -u` creates `state/<label>.json` if it doesn't exist
  [verify](../tests/ralph/run-tests.sh#test_plan_update_creates_state_json)
- [ ] `ralph plan -u -h <label>` updates hidden spec
  [judge](../tests/judges/ralph-workflow.sh#test_plan_update_hidden)
- [ ] `ralph plan` runs Claude in wrapix container with base profile
  [judge](../tests/judges/ralph-workflow.sh#test_plan_runs_in_container)
- [ ] `ralph todo` creates molecule and stores ID in state JSON
  [verify](../tests/ralph/run-tests.sh#test_run_closes_issue_on_complete)
- [ ] `ralph todo` (new mode) creates tasks from `specs/<label>.md`
  [verify](../tests/ralph/run-tests.sh#test_run_closes_issue_on_complete)
- [ ] `ralph todo` detects update mode from `base_commit` presence
  [verify](../tests/ralph/run-tests.sh#test_todo_update_detection)
- [ ] `ralph todo` computes `git diff` between `base_commit` and HEAD for spec changes
  [verify](../tests/ralph/run-tests.sh#test_todo_git_diff)
- [ ] `ralph todo` errors on uncommitted spec changes
  [verify](../tests/ralph/run-tests.sh#test_todo_uncommitted_error)
- [ ] `ralph todo` stores `base_commit` only after container exits with `RALPH_COMPLETE`
  [verify](../tests/ralph/run-tests.sh#test_todo_sets_base_commit)
- [ ] `ralph todo` does not update `base_commit` on container failure
  [verify](../tests/ralph/run-tests.sh#test_todo_no_base_commit_on_failure)
- [ ] `ralph todo` exits early with message when tier 1 finds no spec changes
  [verify](../tests/ralph/run-tests.sh#test_todo_no_changes_exit)
- [ ] `ralph todo --since <commit>` overrides `base_commit` and forces git-diff mode
  [verify](../tests/ralph/run-tests.sh#test_todo_since_flag)
- [ ] `ralph todo --since <invalid>` errors with clear message
  [verify](../tests/ralph/run-tests.sh#test_todo_since_invalid_commit)
- [ ] `ralph todo` falls back to molecule-based diff when `base_commit` is orphaned
  [verify](../tests/ralph/run-tests.sh#test_todo_orphaned_commit_fallback)
- [ ] `ralph todo` falls back to molecule-based diff when no `base_commit` but molecule exists
  [verify](../tests/ralph/run-tests.sh#test_todo_molecule_fallback)
- [ ] `ralph todo` discovers molecule from the pinned-context file (`docs/README.md` by default) when no state file exists (tier 3)
  [verify](../tests/ralph/run-tests.sh#test_todo_readme_discovery)
- [ ] `ralph todo` reconstructs `state/<label>.json` after README discovery
  [verify](../tests/ralph/run-tests.sh#test_todo_readme_state_reconstruction)
- [ ] `ralph todo` falls through to tier 4 when README has no molecule for the spec
  [verify](../tests/ralph/run-tests.sh#test_todo_readme_no_molecule_fallthrough)
- [ ] `ralph todo` falls through to tier 4 when README molecule ID is stale/invalid
  [verify](../tests/ralph/run-tests.sh#test_todo_readme_stale_molecule_fallthrough)
- [ ] `discover_molecule_from_readme` correctly parses the Beads column from the pinned-context file (`docs/README.md` by default)
  [verify](../tests/ralph/run-tests.sh#test_discover_molecule_from_readme)
- [ ] `discover_molecule_from_readme` returns empty string when spec not in README
  [verify](../tests/ralph/run-tests.sh#test_discover_molecule_not_in_readme)
- [ ] Reconstructed state file has correct label, spec_path, molecule; no base_commit; empty companions
  [verify](../tests/ralph/run-tests.sh#test_todo_readme_reconstructed_state_schema)
- [ ] `ralph todo` uses new mode when no state file and no molecule in README (tier 4)
  [verify](../tests/ralph/run-tests.sh#test_todo_new_mode_fallback)
- [ ] `ralph todo` runs `bd dolt push` inside container after `RALPH_COMPLETE`
  [verify](../tests/ralph/run-tests.sh#test_todo_dolt_push_in_container)
- [ ] `ralph todo` runs `bd dolt pull` on host after container exits with `RALPH_COMPLETE`
  [verify](../tests/ralph/run-tests.sh#test_todo_dolt_pull_after_complete)
- [ ] `ralph todo` emits informational warning on host when post-sync task count did not increase
  [verify](../tests/ralph/run-tests.sh#test_todo_post_sync_warning)
- [ ] `ralph todo` still advances `base_commit` and commits spec/README despite sync warning
  [verify](../tests/ralph/run-tests.sh#test_todo_advances_base_commit_on_warning)
- [ ] `ralph todo` warning message includes `bd list` check command and `--since` recovery hint
  [verify](../tests/ralph/run-tests.sh#test_todo_warning_includes_recovery_hints)
- [ ] `ralph plan` runs `bd dolt push` inside container after `RALPH_COMPLETE`
  [verify](../tests/ralph/run-tests.sh#test_plan_dolt_push_in_container)
- [ ] `ralph run --once` runs `bd dolt push` inside container after `RALPH_COMPLETE`
  [verify](../tests/ralph/run-tests.sh#test_run_once_dolt_push_in_container)
- [ ] `ralph run` (continuous) runs `bd dolt push` after each `bd close` and at loop exit
  [verify](../tests/ralph/run-tests.sh#test_run_continuous_dolt_push)
- [ ] `ralph run` host-side block runs `bd dolt pull` after container exits
  [verify](../tests/ralph/run-tests.sh#test_run_dolt_pull_after_complete)
- [ ] `ralph todo` runs Claude in wrapix container with base profile
  [judge](../tests/judges/ralph-workflow.sh#test_todo_runs_in_container)
- [ ] `ralph todo` LLM assigns `profile:X` labels per-task based on implementation needs
  [verify](../tests/ralph/run-tests.sh#test_run_profile_selection)
- [ ] `todo-new.md` instructs LLM to write molecule ID to the pinned-context file (`docs/README.md` by default) Beads column
  [judge](../tests/judges/ralph-workflow.sh#test_todo_new_writes_readme_beads)
- [ ] `todo-update.md` instructs LLM to fill in README Beads column if empty
  [judge](../tests/judges/ralph-workflow.sh#test_todo_update_fills_readme_beads)
- [ ] `state/<label>.json` no longer contains `update` or `hidden` fields
  [verify](../tests/ralph/run-tests.sh#test_state_json_schema)
- [ ] `todo-update.md` works with `SPEC_DIFF` (tier 1) when diff is available
  [judge](../tests/judges/ralph-workflow.sh#test_todo_update_with_diff)
- [ ] `todo-update.md` works with `EXISTING_TASKS` (tier 2) when no diff available
  [judge](../tests/judges/ralph-workflow.sh#test_todo_update_with_tasks)
- [ ] `run.md` template handles already-implemented tasks (close and move on)
  [judge](../tests/judges/ralph-workflow.sh#test_run_already_implemented)
- [ ] `read_manifests` errors if a companion directory does not exist
  [verify](../tests/ralph/run-tests.sh#test_read_manifests_missing_directory)
- [ ] `read_manifests` errors if a companion directory lacks `manifest.md`
  [verify](../tests/ralph/run-tests.sh#test_read_manifests_missing_manifest)
- [ ] `read_manifests` returns empty string when no companions declared
  [verify](../tests/ralph/run-tests.sh#test_read_manifests_empty)
- [ ] `read_manifests` wraps each manifest in `<companion path="...">` tags
  [verify](../tests/ralph/run-tests.sh#test_read_manifests_format)
- [ ] `{{COMPANIONS}}` is available in plan-update, todo-new, todo-update, and run templates
  [verify](../tests/ralph/run-tests.sh#test_companion_template_variable)
- [ ] Local template overlay can override `partial/companions-context.md`
  [verify](../tests/ralph/run-tests.sh#test_companion_partial_override)
- [ ] `ralph run` reads `profile:X` label from bead and uses that profile
  [verify](../tests/ralph/run-tests.sh#test_run_profile_selection)
- [ ] `ralph run --profile=X` overrides bead profile label
  [verify](../tests/ralph/run-tests.sh#test_run_profile_selection)
- [ ] `ralph run` falls back to base profile when no label present
  [verify](../tests/ralph/run-tests.sh#test_run_profile_selection)
- [ ] `ralph run --once` completes single issues with blocked-vs-waiting guidance
  [verify](../tests/ralph/run-tests.sh#test_run_closes_issue_on_complete)
- [ ] `ralph run` (continuous) runs on host, spawning containerized work per issue
  [verify](../tests/ralph/run-tests.sh#test_run_loop_processes_all)
- [ ] `ralph check` validates all templates and partials
  [verify](../tests/ralph/run-tests.sh#test_check_valid_templates)
- [ ] `ralph tune` (interactive) identifies correct template and makes edits
  [judge](../tests/judges/ralph-workflow.sh#test_tune_interactive)
- [ ] `ralph tune` (integration) ingests diff and interviews about changes
  [judge](../tests/judges/ralph-workflow.sh#test_tune_integration)
- [ ] `ralph sync --diff` shows local template changes vs packaged
  [verify](../tests/ralph/run-tests.sh#test_diff_local_modifications)
- [ ] `ralph sync` updates templates and backs up customizations
  [verify](../tests/ralph/run-tests.sh#test_sync_backup)
- [ ] `ralph sync --dry-run` previews without executing
  [verify](../tests/ralph/run-tests.sh#test_sync_dry_run)
- [ ] `nix flake check` includes template validation
  [verify](../tests/ralph/run-tests.sh#test_check_exit_codes)
- [ ] Templates use Nix-native definitions with static validation
  [verify](../tests/ralph/run-tests.sh#test_render_template_basic)
- [ ] Partials work via `{{> partial-name}}` markers
  [verify](../tests/ralph/run-tests.sh#test_plan_template_with_partials)
- [ ] `state/current.json` replaced by per-label `state/<label>.json` files
- [ ] `state/current` (plain text) tracks the active label
- [ ] `ralph use <name>` switches active label with validation
- [ ] `ralph todo --spec <name>` operates on the named workflow
- [ ] `ralph run --spec <name>` operates on the named workflow
- [ ] `ralph run` reads spec once at startup and is unaffected by later `ralph use`
- [ ] `ralph status --all` shows summary of all active workflows
- [ ] `ralph status --spec <name>` shows specific workflow status
- [ ] `ralph logs --spec <name>` shows specific workflow logs
- [ ] Serial workflow (no `--spec` flag) continues to work as before
- [ ] Clear error messages when `state/current` is missing and no `--spec` given
- [ ] `ralph plan`, `todo`, `run --once`, and `check` each write `repin.sh` and `claude-settings.json` to `.wrapix/ralph/runtime/<label>/` before launching the container
  [verify](../tests/ralph/run-tests.sh#test_repin_hook_files_written)
- [ ] Settings fragment registers a `SessionStart` hook with matcher `"compact"` pointing at `repin.sh`
  [verify](../tests/ralph/run-tests.sh#test_repin_hook_settings_shape)
- [ ] `repin.sh` emits `hookSpecificOutput.additionalContext` JSON with the condensed re-pin
  [verify](../tests/ralph/run-tests.sh#test_repin_script_output)
- [ ] Re-pin content excludes the full spec body, companion manifest bodies, and full issue description
  [verify](../tests/ralph/run-tests.sh#test_repin_content_is_condensed)
- [ ] Re-pin content stays under 10KB
  [verify](../tests/ralph/run-tests.sh#test_repin_content_size)
- [ ] `lib/sandbox/linux/entrypoint.sh` merges `$RALPH_RUNTIME_DIR/claude-settings.json` into `~/.claude/settings.json` when the env var is set and the file exists
  [verify](../tests/ralph/run-tests.sh#test_entrypoint_merges_ralph_settings)
- [ ] Merge concatenates hook arrays rather than replacing (ralph `SessionStart[compact]` coexists with sandbox `Notification` hook)
  [verify](../tests/ralph/run-tests.sh#test_entrypoint_merge_concatenates_hooks)
- [ ] Runtime directory is removed by a trap on both success and failure paths
  [verify](../tests/ralph/run-tests.sh#test_runtime_dir_cleanup)
- [ ] `.wrapix/ralph/runtime/` is listed in `.gitignore`
  [verify](../tests/ralph/run-tests.sh#test_runtime_dir_gitignored)
- [ ] Host-side commands (`status`, `logs`, `check -t`, `msg`, `tune`, `sync`, `use`) do not create a runtime directory or register the hook
  [verify](../tests/ralph/run-tests.sh#test_host_commands_no_repin_hook)
- [ ] Compacted session receives the re-pin on next model turn
  [judge](../tests/judges/ralph-workflow.sh#test_repin_after_compaction)
- [ ] `ralph run` does NOT `git push` on its own; per-bead commits land without pushing
  [verify](../tests/ralph/run-tests.sh#test_run_does_not_push)
- [ ] `ralph run` exec-s `ralph check` when the molecule reaches completion
  [verify](../tests/ralph/run-tests.sh#test_run_execs_check_on_complete)
- [ ] `ralph check` (no flags) runs the post-loop review against the resolved spec
  [verify](../tests/ralph/run-tests.sh#test_check_default_runs_review)
- [ ] `ralph check` runs `bd dolt push` inside container after `RALPH_COMPLETE` so fix-up/clarify beads reach the host
  [verify](../tests/ralph/run-tests.sh#test_check_dolt_push_in_container)
- [ ] `ralph check` runs `bd dolt pull` on host after container exits, before re-counting beads
  [verify](../tests/ralph/run-tests.sh#test_check_dolt_pull_before_recount)
- [ ] `ralph check` invokes `git push` + `beads-push` only on RALPH_COMPLETE with no new beads and no `ralph:clarify`
  [verify](../tests/ralph/run-tests.sh#test_check_push_gate_clean)
- [ ] `ralph check` exec-s `ralph run` when it creates fix-up beads without a clarify (auto-iteration)
  [verify](../tests/ralph/run-tests.sh#test_check_auto_iterates_via_run)
- [ ] `ralph check` stops without pushing when it sets `ralph:clarify` on any bead
  [verify](../tests/ralph/run-tests.sh#test_check_clarify_stops_push)
- [ ] `ralph check -t` remains a standalone template validator that does not invoke Claude
  [verify](../tests/ralph/run-tests.sh#test_check_templates_no_claude)
- [ ] `check.md` template includes the three-paths-principle section as invariant-clash guidance
  [verify](../tests/ralph/run-tests.sh#test_check_template_has_three_paths)
- [ ] `check.md` instructs the reviewer to propose contextual options per clash, not a fixed A/B/C
  [judge](../tests/judges/ralph-workflow.sh#test_check_contextual_options)
- [ ] Reviewer creates beads with `ralph:clarify` and proposed options in description for each detected clash
  [judge](../tests/judges/ralph-workflow.sh#test_check_clarify_bead_shape)
- [ ] `plan-update.md` instructs the LLM to detect invariant clashes during the interview and ask the user before committing a change
  [judge](../tests/judges/ralph-workflow.sh#test_plan_update_invariant_clash_detection)
- [ ] `plan-new.md` and `plan-update.md` include the `interview-modes` partial
  [verify](../tests/ralph/run-tests.sh#test_plan_templates_include_interview_modes)
- [ ] `interview-modes.md` documents "one by one" and "polish the spec" with loose-matching guidance
  [verify](../tests/ralph/run-tests.sh#test_interview_modes_partial_content)
- [ ] LLM responds to "one by one" (and close variants) with one-question-at-a-time + suggested defaults
  [judge](../tests/judges/ralph-workflow.sh#test_plan_one_by_one_mode)
- [ ] LLM responds to "polish the spec" (and close variants) with a full read-through + specific edits proposed
  [judge](../tests/judges/ralph-workflow.sh#test_plan_polish_mode)
- [ ] Full loop `run → check → run → check → push` terminates cleanly when no more issues are found
  [verify](../tests/ralph/run-tests.sh#test_run_check_loop_terminates)
- [ ] `ralph check` escalates to `ralph:clarify` and stops after `loop.max-iterations` unsuccessful iterations
  [verify](../tests/ralph/run-tests.sh#test_check_iteration_cap_escalates)
- [ ] Iteration counter persists in `state/<label>.json` and resets on clean RALPH_COMPLETE or clarify clear
  [verify](../tests/ralph/run-tests.sh#test_iteration_counter_persistence)
- [ ] `ralph check` push failures (non-fast-forward, detached HEAD, beads-push failure) exit non-zero with recovery hints
  [verify](../tests/ralph/run-tests.sh#test_check_push_failure_modes)
- [ ] Container-executed ralph commands pass `--env RALPH_RUNTIME_DIR=…` to wrapix; entrypoint only merges the settings fragment when the env var is set
  [verify](../tests/ralph/run-tests.sh#test_runtime_dir_env_propagation)
- [ ] `ralph todo` clears `implementation_notes` from state JSON when it advances `base_commit` on RALPH_COMPLETE
  [verify](../tests/ralph/run-tests.sh#test_todo_clears_implementation_notes)
- [ ] `ralph msg` reply prints a `Resume with: ralph run` hint on successful clarify clear
  [verify](../tests/ralph/run-tests.sh#test_msg_reply_resume_hint)
- [ ] Workflow Phases diagram in the spec reflects `plan → todo → run → check → (done + push)`

## Out of Scope

- Cross-workflow file conflicts (user's responsibility to pick non-overlapping features)
- Workflow locking or mutual exclusion
- Limiting the number of concurrent workflows
- Automated testing integration
- PR creation automation
- Formula-based workflows (Ralph uses specs, not formulas)
- Cross-repo automation for template propagation (manual diff + tune)
