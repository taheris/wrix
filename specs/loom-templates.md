# Loom Templates

Askama template engine, partials inventory, per-phase pinning policy, and
snapshot-test contract for the Loom workflow.

## Problem Statement

Loom's workflow phase prompts (`plan`, `todo`, `run`, `check`, `msg`) are
rendered from Askama templates at compile time. The template surface is its
own concern: which partials exist, which template renders which partial in
which phase, which context struct each template binds to, what the snapshot
gate looks like. Folding all of this into [loom-harness.md](loom-harness.md)
made that spec do double duty (platform plumbing + per-prompt content). This
spec owns the prompt surface; [loom-harness.md](loom-harness.md) owns the
crate that builds it, the workspace plumbing, and the runtime that consumes
rendered prompts.

## Requirements

### Functional

1. **Compiled templates.** Every workflow phase prompt is an Askama template
   compiled into the binary. Template correctness — every variable used in
   the template body has a matching field on its typed context struct — is
   verified at compile time. There is no per-project mustache copy to sync,
   back up, diff, or tune; template updates ship via a new loom release.
2. **Template files.** One template per phase plus per-mode variants:
   `plan_new.md`, `plan_update.md`, `todo_new.md`, `todo_update.md`,
   `run.md`, `check.md`, `msg.md`.
3. **Partials.** Reusable fragments included via `{% include
   "partial/<name>.md" %}`. Current set:
   - `context_pinning.md` — pin the project-overview file (`pinned_context`)
   - `style_rules.md` — pin the Rust/style-rules file (`style_rules`); see
     [Pinning Policy](#pinning-policy)
   - `spec_header.md` — render spec label + path + active molecule
   - `companions_context.md` — list companion paths declared on the spec
   - `scratchpad.md` — pin the per-session scratchpad path
   - `exit_signals.md` — document the `LOOM_*` exit markers the phase
     accepts
   - `interview_modes.md` — describe the "one by one" / "polish the spec"
     interview sub-modes
   - `invariant_clash.md` — describe the invariant-clash awareness scan
   - `review_rubric.md` — the live-path / mock-discipline rubric used by
     the per-diff `loom review` step (see [loom-gate.md](loom-gate.md))
   - `sibling_spec_editing.md` — authorize cross-spec edits during a
     planning session; see [Sibling-Spec Editing](#sibling-spec-editing)
4. **Typed context struct per template.** Each `.md` template has a
   matching `#[derive(Template)]` struct in the same crate, with one
   field per variable used in the body. Missing variables fail the
   build; unused fields trigger the workspace's `unused` lint (see
   [`docs/style-rules.md`](../docs/style-rules.md) RS-3).
5. **Per-phase pinning policy.** Each partial is included by an explicit
   set of templates; the matrix is documented in
   [Pinning Policy](#pinning-policy). `style_rules.md` is pinned only in
   `run.md` and `check.md` — the two phases that write or evaluate Rust
   code.
6. **Agent-output markers.** Agent-generated content rendered back into a
   prompt (`previous_failure`, `title`, `description`, `existing_tasks`)
   is delimited with `<agent-output>` / `</agent-output>` markers so the
   receiving agent can distinguish injected content from system
   instructions. This is a best-effort prompt-injection mitigation; the
   real trust boundary is the container.
7. **Snapshot tests.** Every template × representative-input combination
   has an `insta` snapshot test in
   `loom/crates/loom-templates/tests/snapshots.rs`. Snapshots are the
   contract surface; layout drift slips silently past substring asserts.
   Updates require an explicit `snapshot updated because: <reason>` line
   in the PR description (`docs/style-rules.md` TST-4).

### Non-Functional

1. **Compile-time validation.** Template syntax errors, undefined
   variables, and missing partial files all fail `cargo build` — not
   discovered at runtime.
2. **Rust style.** This crate follows `docs/style-rules.md` RS-1..RS-16
   like the rest of the workspace.

## Architecture

### Template Variables

Sourced from agent-side decomposition; each is bound to a typed field on
the relevant context struct. `String`-typed values arriving from beads or
config flow through the parse-don't-validate boundary in
[loom-harness.md](loom-harness.md#parse-dont-validate).

| Variable | Type | Used By |
|----------|------|---------|
| `pinned_context` | `String` | all (via `partial/context_pinning.md`) |
| `style_rules` | `String` | `run`, `check` (via `partial/style_rules.md`) |
| `label` | `SpecLabel` | all |
| `spec_diff` | `Option<String>` | `todo_update` |
| `existing_tasks` | `Option<String>` | `todo_update` |
| `companion_paths` | `Vec<String>` | `plan_update`, `todo_*`, `run`, `check`, `msg` |
| `clarify_beads` | `Vec<ClarifyBead>` | `msg` |
| `implementation_notes` | `Vec<String>` | `todo_new`, `todo_update` |
| `molecule_id` | `Option<MoleculeId>` | `todo_update`, `run` |
| `issue_id` | `Option<BeadId>` | `run` |
| `title` | `Option<String>` | `run` |
| `description` | `Option<String>` | `run` |
| `beads_summary` | `Option<String>` | `check` |
| `base_commit` | `Option<String>` | `check` |
| `previous_failure` | `Option<String>` | `run` (retry only, truncated to 4000 chars) |
| `exit_signals` | `String` | all (via `partial/exit_signals.md`) |
| `scratchpad_path` | `String` | all (via `partial/scratchpad.md`) |

`implementation_notes` is sourced from `SELECT text FROM notes WHERE
spec_label = ? AND kind = 'implementation' ORDER BY id` — see *Notes
lifecycle* in [loom-harness.md](loom-harness.md#sqlite-state-store).

### Pinning Policy

Each partial is included by an explicit set of templates. The matrix:

| Partial | `plan_new` | `plan_update` | `todo_new` | `todo_update` | `run` | `check` | `msg` |
|---|:-:|:-:|:-:|:-:|:-:|:-:|:-:|
| `context_pinning.md` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `style_rules.md` |  |  |  |  | ✓ | ✓ |  |
| `spec_header.md` |  | ✓ | ✓ | ✓ | ✓ | ✓ |  |
| `companions_context.md` |  | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `scratchpad.md` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `exit_signals.md` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `interview_modes.md` | ✓ | ✓ |  |  |  |  |  |
| `invariant_clash.md` |  | ✓ |  |  |  |  |  |
| `review_rubric.md` |  |  |  |  |  | ✓ |  |
| `sibling_spec_editing.md` |  | ✓ |  |  |  |  |  |

**`style_rules.md` is pinned only in `run` and `check`** — the two phases
that write or evaluate Rust code. Other phases (planning, decomposition,
clarify resolution) don't write code, so pinning the rules there would
inflate prompt size without buying enforcement.

**`partial/style_rules.md` body:**

```markdown
## Style Rules

Read `{{ style_rules }}` — these are the rules every change must
follow. Cite a rule by ID (e.g. RS-9, SH-2, COM-1) when explaining
a fix.
```

The agent reads the file at the resolved path and applies the rules
mechanically; the partial is the pin, the doc is the content.

### Sibling-Spec Editing

`partial/sibling_spec_editing.md` is included only by `plan_update.md`. It
tells the planning agent that:

1. The label named on the `loom plan -u` flag is the **anchor**; it owns
   the session state file.
2. During this session, the agent may read and edit **any spec in
   `specs/`** when a change cross-cuts sibling specs. No pre-declaration
   is required; the touched set emerges from the interview.
3. **Creating a new sibling spec is also a valid outcome** when the
   planner judges that a section warrants its own spec. In that case the
   planner may allocate a tracking epic for the new sibling
   (`bd create --type=epic --title="..."`) and record its ID in
   `docs/README.md`. This is the **one carve-out** from the general
   "no bead creation during planning" rule — the epic is part of the
   split's bookkeeping (so the new spec has an index row), not net-new
   implementation scoping. Implementation beads for the new spec are
   created later, by `loom todo`.
4. **Commits are not automatic.** Planning sessions edit specs in place
   but do **not** commit those edits. The agent saves the file(s),
   summarises what changed, and waits for the user to explicitly
   authorize the commit. Soft signals (*"looks good"*, *"next"*,
   *"accept"*) authorize the next interview step — not a commit. The
   commit happens only when the user uses unambiguous language
   (*"commit"*, *"ship it"*, *"land the changes"*). This avoids
   premature commits that force iteration via `git revert` or
   amend-rewrite. The same discipline applies to `git push`,
   `beads-push`, and any other operation that mutates shared state.

### Snapshot Test Contract

Every template × representative input set has an `insta` snapshot. The
rendered template body is the contract we ship to the agent; layout drift
slips silently past substring assertions. Snapshots surface the diff in
PR review.

- One snapshot per typed context struct, named after the test function
  via `insta::assert_snapshot!`'s default file naming.
- Snapshot updates require an explicit `snapshot updated because:
  <reason>` line in the PR description (`docs/style-rules.md` TST-4).
- The crate-root `#![allow(clippy::unwrap_used, clippy::expect_used,
  clippy::panic)]` in `tests/snapshots.rs` is replaced by the workspace
  `clippy.toml` `allow-*-in-tests` flags (`docs/style-rules.md` RS-3).

## Affected Files

### New

| File | Role |
|------|------|
| `loom/crates/loom-templates/templates/partial/style_rules.md` | Pins `docs/style-rules.md` for the agent (run + check only) |

### Modified

| File | Change |
|------|--------|
| `loom/crates/loom-templates/templates/run.md` | Add `{% include "partial/style_rules.md" %}` after `context_pinning.md` |
| `loom/crates/loom-templates/templates/check.md` | Add `{% include "partial/style_rules.md" %}` after `context_pinning.md` |
| `loom/crates/loom-templates/src/run/mod.rs` | Add `style_rules: String` field to `RunContext` |
| `loom/crates/loom-templates/src/check/mod.rs` | Add `style_rules: String` field to `CheckContext` |
| `loom/crates/loom-templates/tests/snapshots.rs` | Drop crate-root `#![allow(...)]`; rely on workspace `clippy.toml`. Update `run` + `check` snapshot fixtures to include `style_rules` field |
| `loom/crates/loom-templates/templates/partial/sibling_spec_editing.md` | Add (1) the new-sibling-spec-creation carve-out paragraph and (2) the commit-discipline paragraph (no auto-commit; wait for explicit user authorization before any git or beads-push command) |
| `loom/crates/loom-driver/src/config/mod.rs` | Add `style_rules: String` field to `LoomConfig` (default `"docs/style-rules.md"`) |
| `loom/crates/loom-workflow/src/run/**` | Thread `style_rules` through `RunContext` construction |
| `loom/crates/loom-workflow/src/check/**` | Thread `style_rules` through `CheckContext` construction |

## Success Criteria

### Engine

- [ ] All workflow templates compile under Askama with their typed
      context structs
  [verify](tests/loom-test.sh::test_askama_templates_compile)
- [ ] Each template has a typed context struct with all required variables
  [verify](tests/loom-test.sh::test_template_context_structs)
- [ ] Templates compile at build time (missing variables are compile errors)
  [verify](tests/loom-test.sh::test_template_compile_time_check)
- [ ] Partials included via Askama's `{% include %}` mechanism
  [verify](tests/loom-test.sh::test_template_partials)
- [ ] Rendered output is stable across runs for identical inputs (verified
      by `insta` snapshots)
  [verify](tests/loom-test.sh::test_template_snapshots_stable)

### Pinning policy

- [ ] `partial/style_rules.md` exists and renders the `style_rules`
      variable
  [verify](tests/loom-test.sh::test_style_rules_partial_exists)
- [ ] `run.md` includes `partial/style_rules.md`
  [verify](tests/loom-test.sh::test_run_pins_style_rules)
- [ ] `check.md` includes `partial/style_rules.md`
  [verify](tests/loom-test.sh::test_check_pins_style_rules)
- [ ] No other phase template (`plan_new`, `plan_update`, `todo_new`,
      `todo_update`, `msg`) includes `partial/style_rules.md`
  [verify](tests/loom-test.sh::test_style_rules_not_pinned_elsewhere)
- [ ] `RunContext` and `CheckContext` carry `style_rules: String`; other
      phase context structs do not
  [verify](tests/loom-test.sh::test_style_rules_field_scope)
- [ ] `LoomConfig.style_rules` defaults to `"docs/style-rules.md"` when
      the field is absent from `.wrapix/loom/config.toml`
  [verify](tests/loom-test.sh::test_loom_config_style_rules_default)
- [ ] `LoomConfig.style_rules = ""` and `LoomConfig.pinned_context = ""`
      are rejected at parse time with
      `ConfigError::EmptyPath { field }` (the field name appears in
      the error); blanking the config does not disable the pin
  [verify](tests/loom-test.sh::test_loom_config_empty_path_rejected)

### Agent-output markers

- [ ] Templates that render agent-generated content delimit it with
      `<agent-output>` / `</agent-output>` markers
  [verify](tests/loom-test.sh::test_agent_output_markers_present)

### Snapshot tests

- [ ] Every template × representative-input combination has an `insta`
      snapshot under `loom/crates/loom-templates/tests/snapshots/`
  [verify](tests/loom-test.sh::test_template_snapshot_coverage)
- [ ] Snapshot tests run under the workspace `clippy.toml` test
      exemptions — no per-file `#![allow(clippy::unwrap_used, ...)]`
  [verify](tests/loom-test.sh::test_snapshots_no_crate_root_allows)

### Sibling-spec editing

- [ ] `partial/sibling_spec_editing.md` documents that creating a new
      sibling spec is a valid planning-session outcome and names the
      bead-allocation carve-out
  [judge](tests/judges/loom.sh::judge_sibling_spec_editing_documents_split)

## Out of Scope

- **Spec-lifecycle CLI commands** — splitting, merging, renaming, and
  superseding specs are decisions made inside a planning session
  (`loom plan -u`), with judgment applied to which sections move, which
  beads reassign, and which cross-refs rewrite. The CLI exposes no
  `loom split` / `loom merge` / `loom rename` / `loom supersede` flag.
- **Per-project template customization** — templates are Askama,
  compiled into the binary. There is no per-project template-fetch
  or template-tune mechanism. Project-specific prompt tweaks happen
  via `pinned_context` / `style_rules` config and per-spec `notes`.
- **Template content changes** — the *rules* themselves live in
  `docs/style-rules.md`; this spec only pins the file, it does not own
  the RS- list.
- **Selective / per-profile rule filtering in the pin** — was considered
  (conditional partial body keyed off `profile: ProfileName`; per-family
  read-range hints) and explicitly deferred. The `partial/style_rules.md`
  pin points at the whole `docs/style-rules.md` file; agents read the
  families relevant to their work. Revisit when prompt-size measurements
  show the unselected pin is materially expensive.

## Configuration

Two pinning-related fields on `LoomConfig`, both pulled from the
workspace `.wrapix/loom/config.toml`:

```toml
# Project overview — pinned in every phase via partial/context_pinning.md
pinned_context = "docs/README.md"

# Rust / project style rules — pinned in run + check via partial/style_rules.md
style_rules = "docs/style-rules.md"
```

Both are `String` paths (project-relative). Empty values are rejected
at config parse time as `ConfigError::EmptyPath { field }` — blanking
the config does not disable the pin. To genuinely drop a pin, remove
the corresponding `{% include %}` from the relevant template (a spec
change, not a config one). Default values keep the bundled docs in
front of the agent with zero configuration.
