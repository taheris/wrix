# Loom Templates

Askama template engine, partials inventory, per-phase pinning
policy, and snapshot-test contract for the Loom workflow.

## Problem Statement

Loom's agent-bearing workflow phase prompts (`plan`, `todo`, `run`,
`review`, `msg`) are rendered from Askama templates compiled into
the binary. `loom gate verify` is deterministic and renders no
template. The template surface is its own concern: which partials
exist, which template renders which partial in which phase, which
context struct each template binds to, and what the snapshot gate
looks like. [loom-harness.md](loom-harness.md) owns the crate that
builds these templates and the runtime that consumes rendered
prompts; this spec owns the prompt surface itself.

## Architecture

### Template Files

One template per phase, plus per-mode variants:

- `plan_new.md`, `plan_update.md`
- `todo_new.md`, `todo_update.md`
- `run.md`, `review.md`, `msg.md`

`loom gate verify` is deterministic — it runs verifiers, audits,
and linters without rendering any agent prompt — so it has no
template. `loom gate review` is the LLM-judged counterpart and
has its own template, distinct from `run.md` because the review
session has different inputs (diff, bead intent, sibling diffs,
prior `loom gate verify` results) and a rubric-walk objective
rather than an implement-the-bead objective.

Each template has a matching `#[derive(Template)]` context struct
in the same crate. The Askama build verifies every variable
referenced in the template body has a matching field on its
context struct — missing variables are compile errors, unused
fields trigger the `unused` workspace lint.

### Partials

Reusable fragments included via `{% include "partial/<name>.md" %}`.
Current set:

| Partial | Purpose |
|---------|---------|
| `context_pinning.md` | Pin the project-overview file (`pinned_context`) |
| `style_rules.md` | Pin the style-rules file (`style_rules`) — see *Style-Rules Partial* below |
| `spec_conventions.md` | Pin the spec-conventions document — see *Spec-Conventions Partial* below |
| `spec_header.md` | Render spec label, path, active molecule |
| `companions_context.md` | List companion paths declared on the spec |
| `scratchpad.md` | Pin the per-session scratchpad path |
| `exit_signals.md` | Document the `LOOM_*` exit markers the phase accepts |
| `interview_modes.md` | Describe the "one by one" / "polish the spec" interview sub-modes |
| `plan_stage_rubric.md` | Gate the planning interview on completeness / coherence / invariant-clash before any commit |
| `invariant_clash.md` | Describe the invariant-clash awareness scan (included transitively via `plan_stage_rubric.md`) |
| `review_rubric.md` | Per-diff review rubric — see [loom-gate.md](loom-gate.md) |
| `sibling_spec_editing.md` | Authorize cross-spec edits during a planning session |

### Pinning Policy

Each partial is included by an explicit set of templates:

| Partial | `plan_new` | `plan_update` | `todo_new` | `todo_update` | `run` | `review` | `msg` |
|---|:-:|:-:|:-:|:-:|:-:|:-:|:-:|
| `context_pinning.md` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `style_rules.md` |  |  |  |  | ✓ | ✓ |  |
| `spec_conventions.md` | ✓ | ✓ |  |  |  |  |  |
| `spec_header.md` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |  |
| `companions_context.md` |  | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `scratchpad.md` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `exit_signals.md` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `interview_modes.md` | ✓ | ✓ |  |  |  |  |  |
| `plan_stage_rubric.md` | ✓ | ✓ |  |  |  |  |  |
| `invariant_clash.md` | ✓ | ✓ |  |  |  |  |  |
| `review_rubric.md` |  |  |  |  |  | ✓ |  |
| `sibling_spec_editing.md` |  | ✓ |  |  |  |  |  |

**`style_rules.md` is pinned only in `run` and `review`** — the two
phases that write or evaluate code (`run` produces it, `review`
judges it). Other phases (planning, decomposition, clarify
resolution) don't write or evaluate code, so pinning the rules
there would inflate prompt size without buying enforcement.

**`spec_conventions.md` is pinned only in `plan_new` and
`plan_update`** — the two phases that author spec content. Other
phases consume specs but don't modify them.

### Template Variables

Each variable is bound to a typed field on the relevant context
struct. `String`-typed values arriving from beads or config flow
through the parse-don't-validate boundary defined in
[loom-harness.md](loom-harness.md#parse-dont-validate).

| Variable | Type | Used By |
|----------|------|---------|
| `pinned_context` | `String` | all |
| `style_rules` | `String` | `run`, `review` |
| `spec_conventions` | `String` | `plan_new`, `plan_update` |
| `label` | `SpecLabel` | all |
| `spec_diff` | `Option<String>` | `todo_update` |
| `existing_tasks` | `Option<String>` | `todo_update` |
| `companion_paths` | `Vec<String>` | `plan_update`, `todo_*`, `run`, `review`, `msg` |
| `clarify_beads` | `Vec<ClarifyBead>` | `msg` |
| `implementation_notes` | `Vec<String>` | `todo_new`, `todo_update` |
| `molecule_id` | `Option<MoleculeId>` | `todo_update`, `run` |
| `issue_id` | `Option<BeadId>` | `run` |
| `title` | `Option<String>` | `run` |
| `description` | `Option<String>` | `run` |
| `previous_failure` | `Option<String>` | `run` (retry only, truncated to 4000 chars) |
| `beads_summary` | `Option<String>` | `review` |
| `base_commit` | `Option<String>` | `review` |
| `exit_signals` | `String` | all |
| `scratchpad_path` | `String` | all |

The newtypes (`SpecLabel`, `MoleculeId`, `BeadId`) are
architecture-bearing types defined in
[loom-harness.md](loom-harness.md#parse-dont-validate); the
template treats them as opaque typed values.

`implementation_notes` is sourced from the state DB's `notes` table
(kind = `implementation`); see *Notes lifecycle* in
[loom-harness.md](loom-harness.md#sqlite-state-store).

### Style-Rules Partial

The `style_rules.md` partial is **rule-family-agnostic**: it
instructs the agent to discover rule families from the pinned
`{{ style_rules }}` document, not from a fixed prefix list. The
template body never enumerates specific prefixes like `RS-` or
`COM-`; downstream consumers of loom maintain their own
`style-rules.md` with their own conventions, and the partial
adapts.

The same agnosticism applies to the `review_rubric.md` partial in
[loom-gate.md](loom-gate.md)'s style-rule-conformance dimension:
the rubric instructs the judge to walk every rule family the pinned
document defines, without enumerating prefixes. Any rule-ID example
in template prose is illustrative (placeholder), not normative.

### Spec-Conventions Partial

The `spec_conventions.md` partial pins
[`docs/spec-conventions.md`](../docs/spec-conventions.md), which
defines what a spec is, what it isn't, and the relationship to
code / verifiers / notes / beads. Planning sessions read it so
authored content complies with the convention; this prevents
implementation leakage, status indicators, and historical
narrative from drifting back into spec markdown.

### Agent-Output Markers

Agent-generated content rendered back into a prompt
(`previous_failure`, `title`, `description`, `existing_tasks`) is
delimited with `<agent-output>` / `</agent-output>` markers so the
receiving agent can distinguish injected content from system
instructions. This is a best-effort prompt-injection mitigation;
the real trust boundary is the container.

### Sibling-Spec Editing

`partial/sibling_spec_editing.md` is included only in
`plan_update.md`. It tells the planning agent:

1. The label named on `loom plan -u` is the **anchor**; it owns
   the session state row.
2. During this session, the agent may read and edit any spec in
   `specs/` when a change cross-cuts sibling specs. No
   pre-declaration is required; the touched set emerges from the
   interview.
3. **Creating a new sibling spec is a valid outcome** when the
   planner judges that a section warrants its own spec. The
   planner may allocate a tracking epic for the new sibling and
   record its index entry. This is the one carve-out from the
   general "no bead creation during planning" rule —
   implementation beads for the new spec are created later by
   `loom todo`.
4. **Commits are never automatic.** Planning sessions edit specs
   in place but do not commit. Soft signals ("looks good",
   "accept") authorize the next interview step, not a commit.
   Commits happen only on unambiguous trigger ("commit", "land the
   plane", "push it"). The same discipline applies to `git push`,
   `beads-push`, and any operation that mutates shared state.

### Snapshot Test Contract

Every template × representative-input combination has an `insta`
snapshot. The rendered body is the contract shipped to the agent;
layout drift slips past substring assertions. Snapshots surface
diffs in PR review. Updates require an explicit
`snapshot updated because: <reason>` line in the PR description
(per the team's testing rules).

## Configuration

Three pinning-related fields on `LoomConfig`, all loaded from the
workspace `.wrapix/loom/config.toml`:

```toml
# Project overview — pinned in every phase
pinned_context = "docs/README.md"

# Style rules — pinned in run and check
style_rules = "docs/style-rules.md"

# Spec-authoring conventions — pinned in plan_new and plan_update
spec_conventions = "docs/spec-conventions.md"
```

All three are project-relative paths. Empty values are rejected at
config parse time as `ConfigError::EmptyPath { field }` — blanking
a config does not disable the pin. To genuinely drop a pin, remove
the corresponding `{% include %}` from the relevant template (a
spec change, not a config one). Defaults keep the bundled
documents in front of the agent with zero configuration.

## Success Criteria

### Engine

- All workflow templates compile under Askama with their typed
  context structs
  [check](cargo build -p loom-templates)
- Each template has a typed context struct with every variable
  in the template body bound as a field
  [test](template_renders_are_byte_stable_across_runs)
- Templates compile at build time — missing variables are compile
  errors, not runtime errors
  [test](template_renders_are_byte_stable_across_runs)
- Partials are included via Askama's `{% include %}` mechanism
  [check](grep -q 'partial/context_pinning' crates/loom-templates/templates/run.md)
- Rendered output is stable across runs for identical inputs,
  verified by `insta` snapshots
  [test](template_renders_are_byte_stable_across_runs)

### Pinning policy

- `style_rules.md` partial renders the `style_rules` variable
  [check](grep -q '{{ style_rules' crates/loom-templates/templates/partial/style_rules.md)
- `run.md` and `review.md` include `style_rules.md`; no other
  phase template does
  [check](cargo run -p loom-walk -- template_pinning_matrix)
- `spec_conventions.md` partial renders the `spec_conventions`
  variable; included only by `plan_new` and `plan_update`
  [check](cargo run -p loom-walk -- template_pinning_matrix)
- `RunContext` and `ReviewContext` carry `style_rules: String`;
  other phase contexts do not
  [check](cargo test -p loom-templates --test render template_renders_are_byte_stable_across_runs)
- `PlanNewContext` and `PlanUpdateContext` carry
  `spec_conventions: String`; other phase contexts do not
  [check](cargo test -p loom-templates --test render template_renders_are_byte_stable_across_runs)
- `LoomConfig.style_rules` defaults to `"docs/style-rules.md"`;
  `LoomConfig.spec_conventions` defaults to
  `"docs/spec-conventions.md"`; `LoomConfig.pinned_context`
  defaults to `"docs/README.md"`
  [test](pin_paths_default_to_bundled_docs)
- Empty string values for any pin path are rejected at parse time
  with `ConfigError::EmptyPath { field }` naming the offending
  field
  [test](empty_pin_path_returns_empty_path_error)
- The `style_rules.md` and `review_rubric.md` partials are
  rule-family-agnostic: their bodies do not enumerate fixed
  prefixes like `SH-` / `RS-` / `COM-`; rule-ID examples in
  template prose are placeholders, not normative
  [check](cargo test -p loom-templates --test render review_renders_style_rule_conformance_walkthrough)
- Every cell of the pinning matrix above matches the actual
  `{% include %}` graph in `loom-templates/templates/` (transitive
  resolution); drift in either direction — `✓` with no include or
  include with no `✓` — fails the audit
  [check](cargo run -p loom-walk -- template_pinning_matrix)

### Agent-output markers

- Templates that render agent-generated content delimit it with
  `<agent-output>` / `</agent-output>` markers
  [test](agent_output_markers_wrap_each_agent_supplied_field)

### Snapshot tests

- Every template × representative-input combination has an `insta`
  snapshot
  [check](cargo test -p loom-templates --test snapshots)
- Snapshot tests run under the workspace clippy test exemptions
  (no per-file `#![allow(clippy::unwrap_used, ...)]`)
  [check](cargo run -p loom-walk -- loom_templates_snapshots_no_crate_root_allow)

### Sibling-spec editing

- `partial/sibling_spec_editing.md` documents that creating a new
  sibling spec is a valid planning-session outcome and names the
  bead-allocation carve-out
  [judge](tests/judges/loom.sh::judge_sibling_spec_editing_documents_split)

## Requirements

### Functional

1. **Compiled templates.** Every workflow phase prompt is an
   Askama template compiled into the binary. Template correctness
   is verified at compile time. No per-project template-fetch or
   template-tune mechanism; template updates ship via a new loom
   release.
2. **One template per phase plus per-mode variants** as enumerated
   in *Template Files* above.
3. **Partials** as enumerated in *Partials* above. Each partial
   declares which templates include it; the matrix in *Pinning
   Policy* is the authoritative listing.
4. **Typed context per template.** Each template has a Rust
   `#[derive(Template)]` struct with one field per variable. The
   variable set is enumerated in *Template Variables*.
5. **Per-phase pinning.** Partial inclusion follows *Pinning
   Policy*; `style_rules.md` is pinned in `run` and `review` only;
   `spec_conventions.md` is pinned in `plan_new` and `plan_update`
   only.
6. **Rule-family agnosticism.** The `style_rules.md` and
   `review_rubric.md` partial bodies discover rule families from
   the pinned `{{ style_rules }}` document. Template bodies do
   not enumerate fixed prefixes.
7. **Agent-output markers.** All agent-generated content rendered
   back into a prompt is wrapped in `<agent-output>` /
   `</agent-output>`.
8. **Snapshot tests.** Every template × representative-input
   combination has an `insta` snapshot.

### Non-Functional

1. **Compile-time validation.** Template syntax errors, undefined
   variables, and missing partial files all fail the build, not
   discovered at runtime.
2. **Style.** Follows the team's
   [`docs/style-rules.md`](../docs/style-rules.md).

## Out of Scope

- **Spec-lifecycle CLI commands.** Splitting, merging, renaming,
  and superseding specs are decisions made inside a planning
  session, with judgment applied to which sections move, which
  beads reassign, and which cross-refs rewrite. The CLI exposes
  no dedicated split / merge / rename / supersede commands.
- **Per-project template customization.** Templates are Askama,
  compiled into the binary. There is no per-project template-fetch
  or template-tune mechanism. Project-specific prompt tweaks
  happen via `pinned_context` / `style_rules` /
  `spec_conventions` configuration and per-spec implementation
  notes.
- **Template content changes.** The *rules* themselves live in
  `docs/style-rules.md`; this spec only pins the file and does not
  own its content. The *conventions* themselves live in
  `docs/spec-conventions.md` similarly.
- **Selective rule filtering in the pin.** The
  `partial/style_rules.md` pin points at the whole document;
  agents read the families relevant to their work. Revisit if
  prompt-size measurements show the unselected pin is materially
  expensive.
