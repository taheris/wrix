# Ralph Review

Review gate: `ralph check` (post-loop reviewer + push gate) and `ralph msg`
(clarify resolution), with the Options Format Contract.

## Problem Statement

After implementation work completes, ralph needs a review step that both
validates the result and controls whether it leaves the machine. `ralph check`
is an independent reviewer that assesses spec compliance, detects invariant
clashes, creates fix-up beads, and owns the `git push` gate. `ralph msg` is the
human-facing counterpart: an interactive Drafter session that resolves the
`ralph:clarify` beads check raises (plus host-side fast paths for view,
fast-reply, and dismiss). The shared Options Format Contract is what makes the
clarify beads legible to both sides. The forward pipeline lives in
[ralph-loop.md](ralph-loop.md); the platform in [ralph-harness.md](ralph-harness.md).

## Requirements

### Functional

1. **Post-Loop Review** — `ralph check` (default mode) is an independent reviewer assessing the full deliverable: spec compliance, code quality, test adequacy, coherence
2. **Invariant-Clash Awareness (review side)** — `ralph check` detects when completed work clashes with an existing invariant and surfaces the clash for a human decision instead of silently choosing a path. Detection is LLM-judgment biased toward asking. Options are *contextual* per clash (not a fixed menu), guided by the three-paths principle: preserve the invariant, keep the change on top of the invariant inelegantly, or change the invariant. (Planning-side clash detection lives in [ralph-loop.md](ralph-loop.md).)
3. **Push Gate** — `ralph run` auto-invokes `ralph check` when the molecule reaches completion. `ralph run` commits work per-bead during the loop but does NOT push; push is owned by `ralph check` and only happens when check emits `RALPH_COMPLETE` with no new beads created. If check creates fix-up beads without a clarify, it auto-invokes `ralph run` to work them, then re-runs itself. Iteration continues until clean RALPH_COMPLETE (→ push), `ralph:clarify` (→ stop, wait for user), or the `loop.max-iterations` cap is reached (→ escalate via `ralph:clarify`)
4. **Clarify Resolution** — `ralph msg` resolves outstanding `ralph:clarify` beads. Scope is **cross-spec by default** — bare `ralph msg` lists every outstanding clarify regardless of which spec produced it; `-s <label>` is an opt-in filter matching `spec:<label>` on the bead. `state/current` is not consulted by `ralph msg`.
   - **List (host, default)** — bare `ralph msg` prints a flat outstanding-clarify table (with a SPEC column) and exits. No Claude, no container. Safe to call from other commands' verdicts
   - **Interactive triage + walk** (container, Claude Drafter) — opt-in via `-c`/`--chat`: the LLM reads every outstanding clarify (cross-spec, or scoped via `-s <label>`), presents a triage summary, walks the user through them in order, drafts resolution notes, and clears the label per bead
   - **Fast paths (host)** — `-n <N>` / `-i <id>` views a bead; `-a <choice>` replies (integer → option lookup, otherwise verbatim); `-d` dismisses
5. **Options Format Contract** — clarify beads that present a choice use a standard markdown shape (`## Options — <summary>` + `### Option N — <title>`). `ralph msg` consumes this format for SUMMARY display, view-mode enumeration, and integer fast-reply; `ralph check` produces this format for every invariant-clash bead

## Workflow Phases

```
run → check → (done + push)
       │        │
       │        └─ git push + beads-push (only on RALPH_COMPLETE + no new beads)
       ├─ Invariant-clash detection → ralph:clarify (stop, wait for ralph msg)
       ├─ Fix-up beads found → exec ralph run (auto-iterate)
       └─ Reviewer reads code + spec; bonds fix-ups to molecule

Auto-iteration loop (run ↔ check), bounded by loop.max-iterations:
run → check → (new beads?) ─┬─ yes + no clarify + under cap → exec ralph run → check → …
                            ├─ yes + no clarify + at cap    → set ralph:clarify (escalate) → stop
                            ├─ yes + clarify                → stop, ralph msg → ralph run → check → …
                            └─ no                           → git push + beads-push → done

Clarify-resolution (ralph msg, cross-spec by default):
ralph msg ─┬─ (no flags)               → host list of every outstanding clarify (all specs)
           ├─ -s <label>                → host list filtered to clarifies labeled spec:<label>
           ├─ -c / --chat               → container (Claude Drafter) → triage + walk (all specs)
           │                              └─ per-bead: draft note → bd update --notes + remove ralph:clarify
           │                              └─ exit: RALPH_COMPLETE (partial progress is clean)
           ├─ -c -s <label>             → interactive, scoped to spec:<label>
           ├─ -n <N> / -i <id>          → host view, no Claude
           ├─ -n <N> -a <int>           → host fast-reply: look up ### Option <int>, write note, clear label
           ├─ -n <N> -a <string>        → host fast-reply: store verbatim, clear label
           └─ -n <N> -d                 → host dismiss: remove label with work-around note
```

See [ralph-harness.md](ralph-harness.md) for the top-level `plan → todo → run → check` pipeline.

## Commands

### `ralph check`

```bash
ralph check                     # Post-loop review of the active spec (default)
ralph check --spec <name>       # Post-loop review of a named spec
ralph check -s <name>           # Short form
ralph check -t                  # Validate templates (no Claude invocation; mutually exclusive with review)
```

`ralph check` has two jobs:
1. **Post-loop review** (default) — the canonical review of completed implementation work; also auto-invoked by `ralph run` at molecule completion and owns the push gate (see below)
2. **Template validation (`-t` / `--templates`)** — a static check runnable anywhere, also wired into `nix flake check`. Defined in [ralph-harness.md](ralph-harness.md).

**Post-loop review (default):**

An independent reviewer agent assesses the full deliverable for the resolved spec (via `--spec` or `state/current`). Runs inside a wrapix container with base profile. Context-pinning and companions are injected via standard partials, plus the compaction re-pin hook (see Compaction Re-Pin in [ralph-harness.md](ralph-harness.md)). Reviewer has full codebase access and reads implementation code, tests, `CLAUDE.md`, and related specs on demand.

Input context (in prompt):
- Spec file(s): the anchor at `specs/<label>.md`, plus any sibling specs referenced by task `spec:<s>` labels (reviewer reads on demand)
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
- Flag invariant clashes with `ralph:clarify` + a bead whose description contains the proposed options (using the Options Format Contract); user answers via `ralph msg`
- Emit RALPH_COMPLETE when the review pass is finished

**Auto-chain and push gate** (host-side control flow in `check.sh`):

1. Capture molecule bead count before invoking the reviewer
2. Run reviewer in container; wait for RALPH_COMPLETE (container runs `bd dolt push` on the way out — see Container bead sync protocol in [ralph-harness.md](ralph-harness.md))
3. Host runs `bd dolt pull`, then re-counts beads + checks for any bead carrying `ralph:clarify`
4. Branch:
   - **Clean** (no new beads, no `ralph:clarify`) → `git push` + `beads-push` → exit 0
   - **Fix-up beads, no clarify, under cap** → increment `iteration_count` in state JSON, `exec ralph run` (auto-iterate — `ralph run` will re-invoke check on molecule completion)
   - **Fix-up beads, no clarify, at cap** → label the newest fix-up beads with `ralph:clarify` (description notes the cap was hit), print summary + `ralph msg` pointer, exit 0. No push
   - **`ralph:clarify` present** → print a summary of outstanding questions + pointer to `ralph msg`, exit 0. No push. User responds via `ralph msg`; next `ralph run` invocation resumes the loop
5. Review failures (no RALPH_COMPLETE, or Claude error) exit non-zero without pushing

**Iteration Cap:**
- `loop.max-iterations = 3` in `config.nix` (per molecule, default 3) bounds the `run ↔ check` auto-iteration
- Each `ralph check` invocation that creates fix-up beads without a clarify increments the iteration counter
- On the 3rd unsuccessful iteration, check labels the most recent fix-up beads with `ralph:clarify` (description notes the iteration cap was hit) and stops — the user picks up via `ralph msg` as usual
- Iteration counter is persisted in `state/<label>.json` and reset on clean RALPH_COMPLETE (push) or when the user clears the clarify via `ralph msg`

**Push failure handling:**
- **Non-fast-forward / rejected** — exit non-zero, print `pull/rebase then re-run ralph check`, leave molecule state unchanged. No automatic retry.
- **Detached HEAD** — refuse to push with a clear error. The user is expected to be on a branch for the full ralph workflow.
- **`beads-push` fails after `git push` succeeds** — exit non-zero with `run beads-push manually` hint. Code commits are on the remote; beads remain local and user-recoverable.

Template: `check.md` in `lib/ralph/template/`. Reuses partials: `context-pinning`, `spec-header`, `companions-context`, `exit-signals`. Variables: `BEADS_SUMMARY`, `BASE_COMMIT`, `MOLECULE_ID`.

Exit signals: RALPH_COMPLETE, RALPH_BLOCKED, RALPH_CLARIFY.

Exit codes: 0 = pass (pushed) or clarify-pending (awaiting user); 1 = review failure or template validation error.

### `ralph msg`

Human interface for resolving agent questions tagged with `ralph:clarify`. The default is a host-side **list** of every outstanding clarify across all specs — safe to call from other commands' verdicts, no Claude, no container. An opt-in **interactive Drafter session** (`-c`/`--chat`, container, Claude) triages outstanding clarifies and walks through them with the user. A set of **non-interactive fast operations** (view, fast-reply, dismiss) covers scripted answers on the host without Claude.

`ralph msg` is **cross-spec by design** — the default scope is "all outstanding clarifies", regardless of which spec produced them or which spec is currently active in `state/current`. `state/current` is not consulted. `-s <label>` is the only way to narrow the list, and it filters by the `spec:<label>` bead label.

#### Command shape

```bash
ralph msg                          # List every outstanding clarify, all specs (host-side, default)
ralph msg -s <label>               # List filtered to clarifies labeled spec:<label> (host-side)
ralph msg -c                       # Interactive triage + walk, all specs (container, Claude)
ralph msg -c -s <label>            # Interactive, scoped to spec:<label>
ralph msg -n <N>                   # View clarify #N (host-side, fast)
ralph msg -i <id>                  # View clarify by bead ID (host-side, fast)
ralph msg -n <N> -a <choice>       # Fast-reply to #N (host-side)
ralph msg -i <id> -a <choice>      # Fast-reply by bead ID (host-side)
ralph msg -n <N> -d                # Dismiss #N (host-side)
ralph msg -i <id> -d               # Dismiss by bead ID (host-side)
```

**Flags:**
- `-c` / `--chat` — launch the interactive Drafter session (container, Claude); without this flag `ralph msg` is list-only
- `-s` / `--spec` — filter to clarifies labeled `spec:<label>`. **Filter only — no default.** Bare `ralph msg` is cross-spec; `-s` opts into a narrower view
- `-n <N>` / `--num <N>` — target clarify by 1-based sequential index (as printed in the list table)
- `-i <id>` / `--id <id>` — target clarify by bead ID (stable reference for scripts or deep links)
- `-a <choice>` / `--answer <choice>` — supply an answer non-interactively; `<choice>` is parsed as integer (option picker, see Options Format Contract) or stored verbatim (free-form text)
- `-d` / `--dismiss` — dismiss without answering; removes `ralph:clarify` with a note that the agent should work around it

#### Sequential index

The list table (printed by bare `ralph msg`, or as the pre-walk triage view of the interactive `-c` session) assigns a 1-based index `#` to each outstanding clarify, ordered by bead creation time ascending across the **entire visible set** (cross-spec by default; scoped when `-s <label>` is supplied). The index is **ephemeral** — valid only until the clarify set or filter changes. Users who see stale numbers should re-run `ralph msg` to refresh.

`-n <N>` and `-i <id>` are interchangeable ways to reference a bead; `-n` is ergonomic for humans working from a just-printed list, `-i` is robust when the clarify set may have changed.

#### List output

`ralph msg` without action flags renders a flat summary of every outstanding clarify across all specs and exits (host-side, no Claude). With `-s <label>`, the same table is filtered to clarifies labeled `spec:<label>` and the SPEC column is dropped. Examples:

```
$ ralph msg
Outstanding clarifies (3):

 #  ID          SPEC        SUMMARY
 1  wx-1thzk.14 profiles    Invariant clash: deterministic judges in [judge] runner
 2  wx-abc1d.7  ralph-loop  Task profile labels — per-task vs per-spec
 3  wx-def2e.3  ralph-loop  Naming convention for discovered tasks

Reply:
  ralph msg -c               # interactive triage + walk (container, Claude)
  ralph msg -s ralph-loop    # filter to one spec
  ralph msg -n 2             # view #2
  ralph msg -n 2 -a 3        # fast-reply: pick option 3 from #2
  ralph msg -n 2 -a "text"   # fast-reply: verbatim answer
  ralph msg -n 2 -d          # dismiss #2
```

```
$ ralph msg -s ralph-loop
Outstanding clarifies for ralph-loop (2):

 #  ID          SUMMARY
 1  wx-abc1d.7  Task profile labels — per-task vs per-spec
 2  wx-def2e.3  Naming convention for discovered tasks
```

**SPEC column** is sourced from the bead's `spec:<label>` label. If a clarify lacks the label, render `—`. The column is shown in the unfiltered (cross-spec) view and dropped under `-s <label>` (where every row would carry the same value).

**SUMMARY column** is sourced from the `## Options — <summary>` header of the clarify bead (see Options Format Contract). Fallback when the header is absent or has no summary text: the bead title.

#### View mode

`ralph msg -n <N>` or `ralph msg -i <id>` with no other action flags prints the bead's summary, question body, and enumerated options, then exits. No container, no Claude. Example:

```
$ ralph msg -n 2
Clarify #2 — wx-def2e
Summary: Task profile labels: per-task vs per-spec

<question body from bead description>

## Options

[1] Assign per-spec
    Store `profile` in state JSON; cost: coarse granularity.
[2] Assign per-task (current)
    LLM picks per-task at creation; cost: no change, but state JSON never reflects the choice.
[3] Hybrid
    Per-spec default + per-task override; cost: two code paths.

Reply:
  ralph msg -n 2 -a 3                # pick option 3
  ralph msg -n 2 -a "free-form text" # custom answer
  ralph msg -n 2 -d                  # dismiss
```

#### Fast-reply

`ralph msg -n <N> -a <choice>` or `ralph msg -i <id> -a <choice>` stores an answer and clears `ralph:clarify` without entering the interactive session.

**Choice parsing:**
- If `<choice>` is a **pure integer**, look up `### Option <choice>` in the bead description (see Options Format Contract). The stored note is a composed string: `Chose option <N> — <title>: <body>` (title and body from the matched option subsection).
- If `<choice>` is anything else, store verbatim in bead notes.

**Error on lookup failure:** if `<choice>` is an integer but no matching `### Option <N>` subsection exists in the bead description, `ralph msg` exits non-zero with:
```
Option <N> not found in <bead-id>. Available options: 1, 2, …
Use -a "text" for a free-form answer.
```
This prevents silent garbage notes when the options format is missing or malformed.

#### Dismiss

`ralph msg -n <N> -d` or `ralph msg -i <id> -d` removes the `ralph:clarify` label with a note indicating the agent should work around the question. Host-side, no Claude.

#### Interactive session

`ralph msg -c` (optionally `-c -s <label>`) launches a containerized Drafter session (base profile, `msg.md` template). Session flow:

1. Container starts; LLM reads every outstanding `ralph:clarify` bead — cross-spec by default, or filtered to `spec:<label>` when `-s` is supplied. `state/current` is not consulted
2. LLM presents a triage summary — one line per bead framed by the `## Options — <summary>` header; this is richer than any static heuristic can produce and supersedes the static SUMMARY fallback for the interactive flow
3. User picks an order (or accepts as-presented)
4. For each bead in turn: LLM summarizes the decision in plain language, anchors on the reviewer's options, answers questions and reads code as needed (Researcher affordances), drafts the final bead note when the user lands on an answer, and asks for confirmation before writing
5. On confirmation, the LLM writes the note via `bd update <id> --notes`, removes `ralph:clarify` via `bd update <id> --remove-label=ralph:clarify`, and moves to the next bead
6. Session ends when the queue is exhausted, the user chooses to stop, or the user dismisses an individual bead mid-walk
7. Emit `RALPH_COMPLETE` — partial progress is clean; remaining clarifies persist for the next `ralph msg` session

**Role stance:** Drafter with Researcher affordances. The LLM does not re-generate options (the reviewer already did); it helps the user *decide* among the existing options and writes a high-quality resolution note.

**Execution context:** wrapix container, base profile. Matches `plan`, `todo`, `check` — full codebase access enables the LLM to read spec, companions, `bd show`, `git log`, `git diff` during the dialogue.

**Exit signals:** `RALPH_COMPLETE` only. The interactive session *is* the human response; there is no external party to block on (partial progress is clean complete), and option generation belongs to the reviewer (not this command).

**Resume hint** (shared with fast-reply paths): on every successful clarify clear (interactive or fast), print `Clarify cleared on <id>. Resume with: ralph run -s <label>`, where `<label>` is read from the cleared bead's `spec:<label>` label. The cleared clarify itself names the only spec we know is unblocked; `state/current` is not consulted. If the bead lacks a `spec:<label>` label (unexpected — `check.md` mandates it), fall back to `Resume with: ralph run`. No automatic resume — the user just decided the answer and is best placed to choose when to relaunch the loop.

#### Bead storage contract

`ralph msg` abstracts bead storage — today it uses bead labels and notes. The interface is ralph-level; the options-format and label conventions (see Options Format Contract) are the only contract consumers and producers share.

## Options Format Contract

Clarify beads that present a choice must enumerate their options in a standard markdown shape so that `ralph msg` can render useful summaries, enumerate options for view mode, and resolve integer choices in fast-reply mode.

**Canonical format:**

```markdown
## Options — <one-line summary of the decision, ≤50 chars>

### Option 1 — <short title>
<body paragraph(s) describing the option, typically including the cost>

### Option 2 — <short title>
<body>

### Option 3 — <short title>
<body>
```

**Contract:**

- The `## Options` heading may carry a trailing summary separated by em-dash `—`, en-dash `–`, single hyphen `-`, or double hyphen `--`. Parsers accept any of these separators; LLMs default to em-dash.
- Summary is optional but recommended; when absent or when the `## Options` section is missing entirely, `ralph msg` falls back to the bead title for the SUMMARY column.
- Options are numbered `### Option N — <title>` where `N` is 1-based sequential. Numbering is required for `-a <int>` lookup to work.
- Each option's body extends from its `### Option N` heading until the next `### Option` heading or the next `##` heading.
- Format is chosen to survive markdown-to-terminal renderers cleanly (plain `1.` numbered lists get compressed by many renderers, losing the space between marker and text).

**Producers:**

- `check.md` (reviewer) MUST use this format when creating a `ralph:clarify` bead for an invariant clash. The three-paths principle (preserve invariant / keep on top inelegantly / change invariant) shapes the options, but the format does not mandate exactly three options or fixed titles — contextual per-clash framings are preferred.
- Other producers (e.g., `run.md` retry-exhaustion clarifies that do not enumerate options) may omit the `## Options` section; consumers fall back to the bead title for SUMMARY display, and fast-reply with `-a <int>` errors out as designed.

**Consumer:**

- `ralph msg` is the sole primary consumer. Both its interactive Drafter session and its host-side view / fast-reply / list rendering read the bead description via `bd show <id> --json` and parse this format.

## Template Content Requirements

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
   - Options-format requirement: emit `## Options — <summary>` + `### Option N — <title>` per the Options Format Contract
   - Handling: for each clash, create a bead whose description contains the proposed options, add the `ralph:clarify` label and the `spec:<label>` label naming the spec the clash lives in (the latter is what `ralph msg -s` filters on and what the resume hint reads), bond to molecule
10. Fix-up bead creation — for issues that don't require human judgment, create beads via `bd create` + `bd mol bond` with appropriate `profile:X` and `spec:<label>` labels
11. `{{> exit-signals}}`

**Variables:** `BEADS_SUMMARY`, `BASE_COMMIT`, `MOLECULE_ID`

**Exit signals:** RALPH_COMPLETE, RALPH_BLOCKED, RALPH_CLARIFY

### msg.md

**Purpose:** Interactive Drafter session for resolving outstanding `ralph:clarify` beads. Invoked by the interactive form of `ralph msg` (`-c`/`--chat`, optionally with `-s <label>`).

**Required sections:**
1. Role statement — "You are helping the user resolve outstanding clarify questions. You are a Drafter: the reviewer has already presented options; your job is to help the user decide and write a high-quality resolution note. You may read the codebase, related specs, and companion files to answer questions, but do not re-generate options."
2. `{{> context-pinning}}`
3. Scope line — `{{SCOPE}}`, a one-line description: either `Scope: all outstanding clarifies (cross-spec)` for bare `-c`, or `Scope: clarifies labeled spec:<label>` when `-c -s <label>` is supplied. Replaces the per-spec `{{> spec-header}}` partial — there is no single anchor spec for a cross-spec session
4. Clarify list — `{{CLARIFY_BEADS}}`, a markdown block containing one entry per outstanding clarify (bead ID, **`spec:<label>` value**, title, `## Options — <summary>` header, enumerated options). The LLM uses this to present the triage summary and walk through beads in order.
6. Session flow instructions:
   1. Present the triage summary — one line per bead, using each bead's `## Options — <summary>` header as the framing (fall back to title if absent)
   2. Ask the user for an order (or accept as-presented)
   3. For each bead: summarize the decision, answer questions, read code as needed, draft the final note and confirm before writing
   4. On confirmation, write the note via `bd update <id> --notes` and remove `ralph:clarify` via `bd update <id> --remove-label=ralph:clarify`
   5. Move to the next bead; stop when the queue is exhausted or the user chooses to stop (partial progress is clean; remaining clarifies persist)
7. Output format for the resolution note — the LLM should write a self-contained note (what was decided + why) so that readers a month later do not need to consult the now-modified clarify bead description to understand the decision
8. `{{> exit-signals}}`

**Variables:** `SCOPE`, `CLARIFY_BEADS`

**Exit signals:** RALPH_COMPLETE only. The interactive session *is* the human response — there is no `RALPH_BLOCKED` (partial progress is clean), and `RALPH_CLARIFY` is nonsensical (the session's purpose is resolving clarifies, not creating them; new clarifies belong to worker or reviewer templates).

## Affected Files

| File | Role |
|------|------|
| `lib/ralph/cmd/check.sh` | Post-loop review (default) and template validation (`-t`); runs `bd dolt push` in container and `bd dolt pull` on host so fix-up/clarify beads created by the reviewer reach the host before re-counting; auto-iteration chain (`exec ralph run` on fix-up beads; `git push` + `beads-push` on clean RALPH_COMPLETE) |
| `lib/ralph/cmd/msg.sh` | Clarify-resolution interface: cross-spec by default (does not consult `state/current`); `-s <label>` filters by `spec:<label>` bead label; flat list with SPEC column (dropped under `-s`); interactive Drafter session (container) via `msg.md`; host-side view, fast-reply (integer option lookup + free-form fallback), and dismiss; list/SUMMARY rendering from `## Options — <summary>` header with title fallback; sequential index (`-n <N>`) alongside bead ID (`-i <id>`); resume hint sources `<label>` from cleared bead's `spec:<label>` |
| `lib/ralph/template/check.md` | Reviewer agent prompt (mandates Options Format Contract for invariant-clash beads, and the `spec:<label>` label so `ralph msg` can filter and the resume hint can target a spec) |
| `lib/ralph/template/msg.md` | Interactive Drafter session prompt for `ralph msg` |
| `lib/ralph/template/default.nix` | Template definitions for check and msg |

## Success Criteria

### Check + push gate

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
- [ ] Full loop `run → check → run → check → push` terminates cleanly when no more issues are found
  [verify](../tests/ralph/run-tests.sh#test_run_check_loop_terminates)
- [ ] `ralph check` escalates to `ralph:clarify` and stops after `loop.max-iterations` unsuccessful iterations
  [verify](../tests/ralph/run-tests.sh#test_check_iteration_cap_escalates)
- [ ] Iteration counter persists in `state/<label>.json` and resets on clean RALPH_COMPLETE or clarify clear
  [verify](../tests/ralph/run-tests.sh#test_iteration_counter_persistence)
- [ ] `ralph check` push failures (non-fast-forward, detached HEAD, beads-push failure) exit non-zero with recovery hints
  [verify](../tests/ralph/run-tests.sh#test_check_push_failure_modes)

### Msg

- [ ] `ralph msg` (no flags) lists every outstanding `ralph:clarify` bead across all specs; `state/current` is not consulted
  [verify](../tests/ralph/run-tests.sh#test_msg_list_cross_spec_default)
- [ ] `ralph msg -s <label>` filters the list to clarifies carrying the `spec:<label>` bead label
  [verify](../tests/ralph/run-tests.sh#test_msg_list_filter_by_spec_label)
- [ ] Cross-spec list output includes a SPEC column sourced from the bead's `spec:<label>` label (rendering `—` when absent); the SPEC column is dropped under `-s <label>`
  [verify](../tests/ralph/run-tests.sh#test_msg_list_spec_column)
- [ ] Sequential index `#` orders the entire visible set (cross-spec by default, scoped when `-s` is supplied) by bead creation time ascending
  [verify](../tests/ralph/run-tests.sh#test_msg_index_cross_spec_ordering)
- [ ] `ralph msg -c` (no `-s`) reads every outstanding clarify regardless of `state/current`; `ralph msg -c -s <label>` scopes to clarifies labeled `spec:<label>`
  [verify](../tests/ralph/run-tests.sh#test_msg_interactive_cross_spec_scope)
- [ ] `ralph msg` reply prints `Resume with: ralph run -s <label>` on successful clarify clear, with `<label>` sourced from the cleared bead's `spec:<label>` label; falls back to bare `ralph run` when the label is absent
  [verify](../tests/ralph/run-tests.sh#test_msg_reply_resume_hint_per_spec)
- [ ] `ralph msg` list SUMMARY column renders the `## Options — <summary>` header value, falling back to bead title when the header or summary is absent
  [verify](../tests/ralph/run-tests.sh#test_msg_list_summary_fallback)
- [ ] `ralph msg -c` launches the interactive Drafter session inside a wrapix container with the base profile, using the `msg.md` template; bare `ralph msg` stays host-side (no container, no Claude)
  [verify](../tests/ralph/run-tests.sh#test_msg_interactive_container)
- [ ] `ralph msg` interactive session presents a triage summary of outstanding clarifies before walking through them
  [judge](../tests/judges/ralph-workflow.sh#test_msg_interactive_triage)
- [ ] `ralph msg` interactive session writes resolution notes via `bd update --notes` and clears `ralph:clarify` via `bd update --remove-label=ralph:clarify` per bead
  [judge](../tests/judges/ralph-workflow.sh#test_msg_interactive_clears_label)
- [ ] `ralph msg` interactive session ending mid-walk is a clean `RALPH_COMPLETE`; unresolved clarifies remain visible in the next session
  [verify](../tests/ralph/run-tests.sh#test_msg_partial_progress_clean)
- [ ] `ralph msg` interactive session exit signals are `RALPH_COMPLETE` only (no `RALPH_BLOCKED`, no `RALPH_CLARIFY`)
  [verify](../tests/ralph/run-tests.sh#test_msg_template_exit_signals)
- [ ] `ralph msg -n <N>` views clarify #N on host without launching a container
  [verify](../tests/ralph/run-tests.sh#test_msg_view_host_only)
- [ ] `ralph msg -i <id>` views a clarify by bead ID on host (equivalent to `-n <N>` but ID-addressed)
  [verify](../tests/ralph/run-tests.sh#test_msg_view_by_id)
- [ ] `ralph msg -n <N> -a <int>` looks up `### Option <int>` in the bead description and writes `Chose option <int> — <title>: <body>` to notes
  [verify](../tests/ralph/run-tests.sh#test_msg_answer_option_lookup)
- [ ] `ralph msg -n <N> -a <string>` (non-integer) writes the string verbatim to notes
  [verify](../tests/ralph/run-tests.sh#test_msg_answer_verbatim)
- [ ] `ralph msg -n <N> -a <int>` exits non-zero with a clear error when `### Option <int>` is missing from the bead description
  [verify](../tests/ralph/run-tests.sh#test_msg_answer_option_missing_errors)
- [ ] `ralph msg -n <N> -d` and `ralph msg -i <id> -d` dismiss a clarify with a work-around note, host-side
  [verify](../tests/ralph/run-tests.sh#test_msg_dismiss)
- [ ] `ralph msg -a <choice>` and `ralph msg -d` without `-n <N>`/`-i <id>` exit non-zero with a clear error instead of silently falling through to list mode
  [verify](../tests/ralph/run-tests.sh#test_msg_answer_dismiss_require_target)
- [ ] `msg.md` template exists, includes `context-pinning` and `exit-signals` partials, and renders the `SCOPE` and `CLARIFY_BEADS` variables (no `spec-header` or `companions-context` partials — there is no single anchor spec for a cross-spec session)
  [verify](../tests/ralph/run-tests.sh#test_msg_template_structure)

### Options format

- [ ] Options format parser accepts em-dash, en-dash, single hyphen, or double hyphen as the separator on `## Options` and `### Option N` headings
  [verify](../tests/ralph/run-tests.sh#test_options_format_separators)
- [ ] `check.md` mandates the `## Options — <summary>` + `### Option N — <title>` format for clarify beads it creates for invariant clashes
  [verify](../tests/ralph/run-tests.sh#test_check_template_options_format)
- [ ] Reviewer-created clarify beads conform to the options format with at least one `### Option N —` subsection
  [judge](../tests/judges/ralph-workflow.sh#test_check_options_format_conformance)

## Out of Scope

- Review across workflows (each molecule reviewed in isolation)
- Automatic clarify resolution — clarifies always require human input (via `ralph msg`)
- Push to branches other than the current upstream — user switches branch before invoking check
- Pre-push CI gating — push is to `origin`; external CI is out of ralph's control
