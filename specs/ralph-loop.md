# Ralph Loop

Forward pipeline: `ralph plan` → `ralph todo` → `ralph run`.

## Problem Statement

The forward half of the ralph workflow turns an idea into implemented code:
planning writes/refines a spec, `todo` decomposes the spec into a beads molecule,
and `run` works the issues to completion. This spec defines those three commands
and the supporting machinery (anchor-driven multi-spec planning, per-spec cursor
fan-out, companion content, retry context, profile selection, cross-machine
recovery). The review gate that guards the push is defined in
[ralph-review.md](ralph-review.md); the platform (state, templates, containers,
compaction re-pin, utilities, bootstrap) is defined in
[ralph-harness.md](ralph-harness.md).

## Requirements

### Functional

1. **Spec Interview** — `ralph plan` initializes a feature and conducts requirements gathering
2. **Plan Modes** — `ralph plan` requires exactly one mode flag:
   - `-n/--new`: New spec in `specs/`
   - `-h/--hidden`: New spec in `state/` (not committed)
   - `-u/--update`: Refine existing spec (combinable with `-h`)
3. **Planning Interview Modes** — During `ralph plan -n` / `plan -u`, the user can request two structured sub-modes by saying phrases like "one by one" (walk open design questions individually with suggested defaults) or "polish the spec" (end-of-session read-through for readability, consistency, and ambiguities). Phrase matching is loose — variations like "let's go through one by one" or "polish this spec" also trigger the respective mode
4. **Anchor-Driven Multi-Spec Planning** — a plan session named by `-u <anchor>` may touch sibling specs in `specs/`:
    - **Anchor** — `ralph plan -u <anchor>` opens the session; session state lives in `state/<label>.json` where `<label>` is the anchor
    - **Siblings** — the LLM may read and edit any spec in `specs/` when a change cross-cuts; no pre-declaration, the set emerges from the interview
    - **Fan-out** — `ralph todo` widens its diff to `git diff <anchor.base_commit> HEAD -- specs/` and computes a per-spec diff using each touched spec's own `base_commit`
    - **Cursor advancement** — on `RALPH_COMPLETE`, every spec that received at least one task has its `base_commit` advanced; sibling state files are created on demand
    - **`--since <commit>`** — overrides only the anchor's cursor; siblings retain their own
5. **Invariant-Clash Awareness (planning side)** — `ralph plan -u` detects when a proposed change clashes with an existing invariant across the anchor and any touched sibling specs, and surfaces the clash for a human decision instead of silently choosing a path. The detection is LLM-judgment biased toward asking. Proposed options are *contextual* (not a fixed menu), guided by the three-paths principle: preserve the invariant, keep the change on top of the invariant inelegantly, or change the invariant. (Review-side clash detection lives in [ralph-review.md](ralph-review.md).)
6. **Molecule Creation** — `ralph todo` converts specs to beads molecules
7. **Issue Work** — `ralph run` processes issues (single with `--once`, continuous by default)
8. **Git-Based Spec Diffing** — `ralph todo` uses `base_commit` in state JSON and `git diff` to detect spec changes, scoped by the anchor's cursor
9. **Cross-Machine State Recovery** — `ralph todo` discovers molecule IDs from the pinned-context file (`docs/README.md` by default) when no local state file exists, reconstructing `state/<label>.json` to avoid duplicate molecule creation
10. **Post-Sync Verification (informational)** — After `bd dolt pull` on the host, `ralph todo` checks whether task count increased and emits a warning with recovery hints if not; `base_commit` always advances on `RALPH_COMPLETE`
11. **Companion Content** — Specs can declare companion directories whose `manifest.md` is injected into templates, giving the LLM awareness of related content it can read on demand

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

**Anchor session (update mode):** `ralph plan -u <anchor>` is anchored on one spec but permits the LLM to edit sibling specs when a change cross-cuts.

- **State** — the anchor's state file (`state/<label>.json`, where `<label>` is the anchor) owns `implementation_notes`, `molecule`, and `iteration_count` regardless of which spec files are edited
- **Sibling edits** — ordinary commits to `specs/<sibling>.md`; no pre-declaration; the touched set emerges from the interview
- **Invariant-clash awareness across siblings** — a change landing in the anchor may clash with an invariant in a sibling; the LLM scans all touched specs for clashes and asks the user before committing, using the three-paths principle
- **Hidden specs (`-u -h`)** — remain single-spec; hidden specs are not in `specs/` and do not participate in the anchor/sibling model

See the `ralph todo` section for how sibling edits are picked up at task-creation time (per-spec cursor fan-out).

### `ralph todo`

```bash
ralph todo                  # Operate on current spec (from state/current)
ralph todo --spec <name>    # Operate on named spec
ralph todo -s <name>        # Short form
ralph todo --since <commit> # Force git-diff mode from specific commit
```

Launches wrapix container with base profile. Reads the anchor's `state/<label>.json` (resolved via `--spec` flag or `state/current`; the resolved label plays the anchor role for this session). Uses four-tier detection to determine mode:

1. **Tier 1 (git diff)**: `base_commit` exists in anchor's state JSON → widen to `git diff <anchor.base_commit> HEAD -- specs/` and apply **per-spec cursor fan-out** (below)
2. **Tier 2 (molecule-based)**: No `base_commit` but molecule in state JSON → LLM compares spec against existing task descriptions (single-spec only; siblings not supported in this tier)
3. **Tier 3 (README discovery)**: No `state/<label>.json` exists for the anchor → look up molecule ID from the pinned-context file (`docs/README.md` by default) Beads column, validate with `bd show`, reconstruct state file, then proceed as tier 2
4. **Tier 4 (new)**: No state file AND no molecule in README (or README molecule is stale/invalid) → full spec decomposition from `specs/<anchor>.md`

**Per-spec cursor fan-out (tier 1):**

Under Anchor-Driven Multi-Spec Planning, a single plan session may edit multiple specs. Each spec owns its own `base_commit` (required for concurrent workflows), so the diff is computed per-spec, not anchor-wide.

1. **Candidate set** — `git diff <anchor.base_commit> HEAD --name-only -- specs/` returns every spec file changed since the anchor's cursor.
2. **Per-spec effective base** — for each candidate spec `<s>`:
   - If `<s>` is the anchor AND `--since <commit>` was supplied → use `<commit>` (the override supersedes the anchor's stored `base_commit` for the anchor's own per-spec diff, not only for the candidate-set computation)
   - Else if `state/<s>.json` exists → use `state/<s>.base_commit`
   - Else → seed from `<anchor>.base_commit` (first time this spec is being tasked)
   - If the effective base is orphaned (detected via `git merge-base --is-ancestor`) → fall back to `<anchor>.base_commit`
3. **Compute per-spec diff** — `git diff <effective_base> HEAD -- specs/<s>.md`.
4. **Feed {spec → diff} map** to the task-creation LLM. Each task gets a `spec:<s>` label identifying which file it implements; all tasks bond to the anchor's molecule.
5. **Fan-out on `RALPH_COMPLETE`** — for every spec that received at least one task, set `state/<s>.base_commit = HEAD`. If `state/<s>.json` did not exist, create it with `{label: <s>, spec_path: "specs/<s>.md", base_commit: HEAD, companions: []}`. Anchor's state file is always updated (molecule ID, cleared `implementation_notes`).

**Flags:**
- `--since <commit>` forces tier 1 with the given commit as the **anchor's cursor only** — applied both to the candidate-set diff (`git diff <commit> HEAD -- specs/`) and to the anchor's own per-spec diff inside the fan-out loop. Sibling specs retain their own `base_commit` values; the flag does not override them. Errors if commit is invalid.

**Constraints:**
- Requires spec changes to be committed — errors if uncommitted changes detected in any spec file in the candidate set
- If tier 1 finds no changes (empty candidate set), exits early: "No spec changes since last task creation"
- When `base_commit` is orphaned (rebase/amend), detected via `git merge-base --is-ancestor`, falls back to tier 2 for that spec
- Hidden spec updates (`-u -h`) use tier 2, single-spec only (hidden specs are not in git and cannot participate in fan-out)
- Tier 3 reconstructs `state/<label>.json` with: label, spec_path (`specs/<label>.md`), molecule (from README), no base_commit, empty companions array

**Profile assignment:** The LLM analyzes each task's requirements and assigns appropriate `profile:X` labels based on implementation needs (e.g., tasks touching `.rs` files get `profile:rust`). This happens per-task, not per-spec.

Stores molecule ID in the anchor's `state/<label>.json`. On `RALPH_COMPLETE`, atomically stores `HEAD` as `base_commit` on every spec that received at least one task and clears the anchor's `implementation_notes` (see Implementation Notes section for lifecycle rationale). Host-side verification checks that tasks synced correctly but is informational only — it does not block `base_commit` advancement or workflow progression.

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
- **Does NOT `git push`** — commits land per-bead during the loop but the push is deferred to `ralph check` (see Push Gate in [ralph-review.md](ralph-review.md))
- **At molecule completion, auto-invokes `ralph check`** so the user isn't required to chain commands manually. The exec replaces the current process (`ralph run` exits through `ralph check`'s exit code)

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

## Git-Based Spec Diffing

Spec-change detection uses `git diff` scoped by the `base_commit` field in state JSON; no intermediate files.

- `ralph plan -u <anchor>` edits `specs/<anchor>.md` directly and may edit sibling specs in `specs/` (LLM commits the changes for git-tracked specs)
- `ralph plan -u -h` edits the hidden spec directly but does NOT commit (single-spec only; no sibling editing)
- `ralph todo` uses four-tier detection with per-spec cursor fan-out in tier 1 (see `ralph todo` command section)
- `compute_spec_diff` helper in `util.sh` encapsulates the four-tier fallback and the per-spec fan-out for tier 1
- `discover_molecule_from_readme` helper in `util.sh` parses the pinned-context file (`docs/README.md` by default) to find molecule IDs by spec label
- `--since <commit>` flag forces tier 1 with the given commit as the **anchor's cursor only** — applied to both the candidate-set diff and the anchor's per-spec diff in fan-out; sibling specs retain their own `base_commit` values
- No explicit migration required — existing specs handled via tier 2/3/4 fallback

### Per-Spec Cursor Fan-Out (Tier 1 details)

Each spec owns its own `base_commit`. A single `ralph plan -u` session may edit multiple specs across multiple commits; fan-out ensures each spec's cursor advances only for the portion of the diff actually tasked.

**Diff computation per candidate spec `<s>`:**

1. Determine effective base:
   - If `state/<s>.json` exists → `state/<s>.base_commit`
   - Else → anchor's `base_commit` (seed)
   - If orphaned → anchor's `base_commit` (fallback)
2. Run `git diff <effective_base> HEAD -- specs/<s>.md`

The candidate set is computed from the **anchor's cursor** (`git diff <anchor.base_commit> HEAD --name-only -- specs/`) — this bounds the set of specs to inspect without scanning the entire `specs/` tree.

**Cursor advancement on RALPH_COMPLETE:**

- For every spec `<s>` that received at least one task, set `state/<s>.base_commit = HEAD`
- If `state/<s>.json` did not exist, create it (`{label, spec_path, base_commit, companions: []}`)
- Anchor's `implementation_notes` cleared atomically with its `base_commit` advancement
- Specs in the candidate set that did NOT receive tasks (e.g., a whitespace-only change the LLM judged non-actionable) do not have their cursor advanced — next run re-inspects that diff

**Worked example:**

Initial: `state/auth.base_commit = X`, `state/auth-ui.base_commit = Y` (Y > X, `auth-ui` was planned independently last week).

During `ralph plan -u auth`, commits land on `auth.md` (C1), `auth-ui.md` (C2, discovered cross-cut), `auth-admin.md` (C3, new sibling with no state file).

`ralph todo` run:
- Candidate set from `X..HEAD -- specs/`: `auth.md`, `auth-ui.md`, `auth-admin.md`
- Per-spec diffs:
  - `auth.md`: `X..HEAD` (anchor's own cursor)
  - `auth-ui.md`: `Y..HEAD` (`auth-ui`'s own cursor — no double-creation of tasks already created last week)
  - `auth-admin.md`: `X..HEAD` (no state yet, seeded from anchor)
- Tasks created across all three with `spec:<label>` labels; all bonded to anchor's molecule
- On RALPH_COMPLETE: all three state files have `base_commit = HEAD`; `auth-admin.json` is newly created

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

## Implementation Notes Lifecycle

`state/<label>.json` has an optional `implementation_notes` array — transient hints captured during planning that inform task creation but should not outlive it.

- **Written** during `ralph plan -u` when the user shares task-creation-shaped context the LLM should preserve for the next `ralph todo` session
- **Read** by `ralph todo` via the `IMPLEMENTATION_NOTES` template variable (todo-new, todo-update)
- **Cleared** atomically with `base_commit` advancement on `ralph todo` RALPH_COMPLETE — notes are task-creation-local; re-running on a new diff starts clean

Notes live with the anchor regardless of which sibling specs the planning session edited.

## Template Content Requirements

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
9. `{{> interview-modes}}` — Documents the "one by one" and "polish the spec" fast phrases (see partial in [ralph-harness.md](ralph-harness.md)). Phrase matching is loose
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
8. Anchor session + sibling-spec editing — The label named on the `-u` flag is the **anchor**; it owns the state file (`state/<label>.json`). The LLM may read and edit any spec in `specs/` when a change cross-cuts — sibling specs are edited in place and committed like the anchor. No pre-declaration is required. `docs/README.md` is the spec index; consult it to locate siblings. Hidden specs (`-u -h`) are single-spec and do not participate in sibling editing
9. Invariant-clash awareness — Before committing a spec change, scan the anchor **and any touched sibling specs** for invariants the change may clash with (architectural decisions, data-structure choices, explicit constraints, non-functional requirements, out-of-scope items). When a potential clash is found, pause the interview and ask the user to pick a path, proposing *contextual* options for the specific clash — guided by the three-paths principle (preserve invariant / keep on top inelegantly / change invariant) but not limited to exactly three or those exact framings. Bias toward asking when uncertain — the cost of asking is low compared to a silent wrong choice
10. Implementation notes guidance — Store transient implementation hints in the anchor's state file `implementation_notes` array, not in any spec. Notes always live with the anchor regardless of which sibling file they apply to
11. Instructions — LLM edits `specs/<label>.md` (anchor) and any touched sibling specs directly during the interview; commits changes at end of session (git-tracked specs only; hidden specs just save file)
12. `{{> interview-modes}}` — Documents the "one by one" and "polish the spec" fast phrases
13. `{{> exit-signals}}`

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
4. Existing spec — Show the anchor spec (`specs/<anchor>.md`) for context (what's already implemented)
5. `{{> companions-context}}` — after existing spec, before diff/tasks section
6. Spec diff or existing tasks — Receives both `{{SPEC_DIFF}}` and `{{EXISTING_TASKS}}`; one is always empty depending on tier. Under per-spec cursor fan-out, `{{SPEC_DIFF}}` may span multiple sibling specs — each diff block is prefixed with its spec path. Instructions: "if SPEC_DIFF is provided, use that to identify new requirements across all listed specs; otherwise use EXISTING_TASKS to compare against the anchor spec and identify gaps."
7. Profile assignment guidance — Assign `profile:X` per-task based on implementation needs
8. Task creation — Create tasks ONLY for new/changed requirements, bond to the anchor's molecule. Label each task with `spec:<s>` identifying which spec file it implements (may differ from the anchor under fan-out)
9. README backfill — If the pinned-context file (`docs/README.md` by default) Beads column is empty for the anchor spec, fill in the molecule ID. Sibling specs touched in this session do not get their own molecule — their tasks bond to the anchor's molecule. Do NOT write the anchor's molecule ID into a sibling's README Beads column; the sibling's row stays empty until it is planned as its own anchor
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

## Affected Files

| File | Role |
|------|------|
| `lib/ralph/cmd/plan.sh` | Feature initialization + spec interview |
| `lib/ralph/cmd/todo.sh` | Issue creation (renamed from ready.sh) |
| `lib/ralph/cmd/run.sh` | Issue work (merged from step.sh + loop.sh), retry logic; commits per-bead but does NOT push; at molecule completion `exec ralph check` |
| `lib/ralph/cmd/util.sh` | `compute_spec_diff` (per-spec fan-out in tier 1), `discover_molecule_from_readme`, `read_manifests` |
| `lib/ralph/template/plan-new.md` | New-spec interview prompt |
| `lib/ralph/template/plan-update.md` | Existing-spec update interview prompt |
| `lib/ralph/template/todo-new.md` | Molecule creation prompt |
| `lib/ralph/template/todo-update.md` | Task-bonding prompt (handles SPEC_DIFF and EXISTING_TASKS) |
| `lib/ralph/template/run.md` | Single-issue implementation prompt (includes `PREVIOUS_FAILURE` for retries) |
| `lib/ralph/template/default.nix` | Template definitions for the above (includes `PREVIOUS_FAILURE` variable) |

## Success Criteria

### Plan

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
- [ ] `ralph plan` runs `bd dolt push` inside container after `RALPH_COMPLETE`
  [verify](../tests/ralph/run-tests.sh#test_plan_dolt_push_in_container)
- [ ] `ralph plan -u <anchor>` permits the LLM to read and edit any spec in `specs/` (sibling-spec editing)
  [judge](../tests/judges/ralph-workflow.sh#test_plan_anchor_sibling_editing)
- [ ] `ralph plan -u -h` (hidden) remains single-spec; no sibling-spec editing
  [verify](../tests/ralph/run-tests.sh#test_plan_update_hidden_single_spec)
- [ ] Invariant-clash detection during planning scans the anchor and any touched sibling specs
  [judge](../tests/judges/ralph-workflow.sh#test_plan_cross_spec_invariant_clash)
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

### Todo

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
- [ ] `ralph todo` runs Claude in wrapix container with base profile
  [judge](../tests/judges/ralph-workflow.sh#test_todo_runs_in_container)
- [ ] `ralph todo` LLM assigns `profile:X` labels per-task based on implementation needs
  [verify](../tests/ralph/run-tests.sh#test_run_profile_selection)
- [ ] `todo-new.md` instructs LLM to write molecule ID to the pinned-context file (`docs/README.md` by default) Beads column
  [judge](../tests/judges/ralph-workflow.sh#test_todo_new_writes_readme_beads)
- [ ] `todo-update.md` instructs LLM to fill in README Beads column if empty
  [judge](../tests/judges/ralph-workflow.sh#test_todo_update_fills_readme_beads)
- [ ] `todo-update.md` works with `SPEC_DIFF` (tier 1) when diff is available
  [judge](../tests/judges/ralph-workflow.sh#test_todo_update_with_diff)
- [ ] `todo-update.md` works with `EXISTING_TASKS` (tier 2) when no diff available
  [judge](../tests/judges/ralph-workflow.sh#test_todo_update_with_tasks)
- [ ] `ralph todo` clears `implementation_notes` from state JSON when it advances `base_commit` on RALPH_COMPLETE
  [verify](../tests/ralph/run-tests.sh#test_todo_clears_implementation_notes)

### Anchor + per-spec fan-out

- [ ] Anchor's `state/<anchor>.json` owns `molecule`, `implementation_notes`, `iteration_count` regardless of which sibling specs are edited
  [verify](../tests/ralph/run-tests.sh#test_anchor_owns_session_state)
- [ ] Sibling state files only hold `label`, `spec_path`, `base_commit`, `companions` (no molecule)
  [verify](../tests/ralph/run-tests.sh#test_sibling_state_shape)
- [ ] `ralph todo` widens tier 1 candidate set to `git diff <anchor.base_commit> HEAD --name-only -- specs/`
  [verify](../tests/ralph/run-tests.sh#test_todo_tier1_candidate_set)
- [ ] `ralph todo` computes per-spec diff using each touched spec's own `base_commit`
  [verify](../tests/ralph/run-tests.sh#test_todo_per_spec_cursor)
- [ ] Sibling with no state file uses the anchor's `base_commit` as its effective base
  [verify](../tests/ralph/run-tests.sh#test_todo_sibling_seed_from_anchor)
- [ ] Orphaned sibling `base_commit` falls back to anchor's `base_commit`
  [verify](../tests/ralph/run-tests.sh#test_todo_sibling_orphan_fallback)
- [ ] Tasks created during a multi-spec session all bond to the anchor's molecule
  [verify](../tests/ralph/run-tests.sh#test_todo_multi_spec_single_molecule)
- [ ] Each task labeled with `spec:<s>` identifying which spec file it implements
  [verify](../tests/ralph/run-tests.sh#test_todo_per_task_spec_label)
- [ ] On RALPH_COMPLETE, `base_commit` advances for every spec that received at least one task
  [verify](../tests/ralph/run-tests.sh#test_todo_cursor_fanout_on_complete)
- [ ] Sibling state file is created on demand if it didn't exist before the fan-out
  [verify](../tests/ralph/run-tests.sh#test_todo_creates_sibling_state_file)
- [ ] `ralph todo --since <commit>` overrides only the anchor's cursor; siblings retain own `base_commit`
  [verify](../tests/ralph/run-tests.sh#test_todo_since_anchor_only)
- [ ] `ralph todo --since <commit>` applies the override to the anchor's own per-spec diff in fan-out, not just the candidate-set computation (so a stale stored `base_commit` on the anchor does not mask the override)
  [verify](../tests/ralph/run-tests.sh#test_todo_since_override_anchor_per_spec_diff)
- [ ] `ralph todo` exits early when tier 1 candidate set is empty ("No spec changes since last task creation")
  [verify](../tests/ralph/run-tests.sh#test_todo_empty_candidate_set_exits)
- [ ] Worked example (anchor + two siblings) produces correct molecule + per-spec cursors
  [judge](../tests/judges/ralph-workflow.sh#test_todo_fanout_worked_example)

### Run

- [ ] `ralph run --once` runs `bd dolt push` inside container after `RALPH_COMPLETE`
  [verify](../tests/ralph/run-tests.sh#test_run_once_dolt_push_in_container)
- [ ] `ralph run` (continuous) runs `bd dolt push` after each `bd close` and at loop exit
  [verify](../tests/ralph/run-tests.sh#test_run_continuous_dolt_push)
- [ ] `ralph run` host-side block runs `bd dolt pull` after container exits
  [verify](../tests/ralph/run-tests.sh#test_run_dolt_pull_after_complete)
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
- [ ] `ralph run` does NOT `git push` on its own; per-bead commits land without pushing
  [verify](../tests/ralph/run-tests.sh#test_run_does_not_push)
- [ ] `ralph run` exec-s `ralph check` when the molecule reaches completion
  [verify](../tests/ralph/run-tests.sh#test_run_execs_check_on_complete)
- [ ] `run.md` template handles already-implemented tasks (close and move on)
  [judge](../tests/judges/ralph-workflow.sh#test_run_already_implemented)

### Companions

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

## Out of Scope

- Anchor/sibling planning for hidden specs (`plan -u -h`) — hidden specs remain single-spec
- Automatic discovery of which specs form a conceptual group — the touched set emerges per-session, not from static declaration
