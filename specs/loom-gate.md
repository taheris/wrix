# Loom Gate

The quality gate. Decides whether code is good enough to ship.

The umbrella concept covering all three stages (plan / per-diff /
standing) and both commands (`loom check` deterministic, `loom review`
LLM-judged). Distinct from the *Verdict Gate* execution layer in
[loom-harness.md](loom-harness.md) — that section owns the per-bead
mechanics that wrap the gate; this spec owns the rubric, the
invariants, the lanes, and the stages.

> **Status: design settled; implementation pending.** All design
> questions raised during the initial design of this spec are
> resolved (see Decisions Log at the bottom). Downstream
> implementation work — renaming `loom-workflow/src/doctor.rs` to
> `check.rs`, wiring the `loom review` subcommand, building the
> `--check=*` audit set — is tracked separately as beads.

## Problem Statement

Before this spec, the review machinery in loom was fragmented across
multiple specs: a *Verdict Gate* in [loom-harness.md](loom-harness.md)
(per-bead, per-diff, narrowly scoped), style rules in
[`docs/style-rules.md`](../docs/style-rules.md) (mechanical lints),
test strategy in [loom-tests.md](loom-tests.md). Each piece did its
part. The gaps *between* them — cross-file incoherence, contracts
no individual bead owned, omissions where no PR was responsible for
the integration — were structurally invisible. That gap was where
the Notes lifecycle orphaned, where `unreachable!()` slipped into
`EventEnvelope::default`, where two parsers' `task_stack` mutex sites
silently swallowed poison. None of those were mysterious bugs; they
were the predictable output of a review surface that had no
consolidated job description.

The empirical case for consolidation (Rothrock 2026, 5,109 gate
checks across a 97-day autonomous-development study): omissions
account for ~49% of rejected work — roughly 4× more common than
incoherence and more common than systematic errors. File-scoped
review detects 0% of cross-file incoherence. Coherence-only or
file-scoped gates structurally cannot catch the dominant failure
modes. The loom review surface before this spec was both
coherence-only *and* file-scoped — which exactly predicted the bugs
above.

This spec gives one place one responsibility: catch divergences before
they ship.

## Invariants — what must never happen

The five failure classes the gate guarantees against. These are the
gate's reason for existing; everything below them is mechanism.

1. **A spec claim is false in the code.** If a spec says X must happen,
   the implementation must make X happen. If a spec bans Y, the
   implementation must not contain Y. Includes multi-component
   contracts: parts {a, b, c} of a lifecycle either all land in the
   implementation, or the unfinished parts have a bonded successor
   doing the remaining work. *Canonical instances: Notes lifecycle
   orphaned; `unreachable!()` in `EventEnvelope::default`;
   `task_stack` mutex-poison swallows.*

2. **A passing verifier is dishonest.** A `[verify]` test that asserts
   a tautology, mocks the thing it claims to test, or passes for the
   wrong reason is itself a divergence — the spec claim it cites is in
   fact unchecked. The gate distinguishes honest from dishonest
   verifiers; *all tests pass* is not synonymous with *the spec is
   enforced*.

3. **A template directs agents toward spec-contradicting behaviour.**
   Planning, decomposition, and review templates are themselves system
   artefacts. They must operate consistently with the specs they drive.
   *Canonical instance: a planning template instructing the agent to
   write `implementation_notes` to a deprecated state-file path,
   against a spec that had already replaced that mechanism with a
   different one — producing cascading damage when the agent followed
   the template literally instead of the spec.*

4. **A divergence sits in the working tree undetected, regardless of
   whether any merge is in flight.** Per-diff review can only see
   what's in the diff. Cross-file gaps, contracts orphaned across
   multiple PRs, pre-existing violations that predate current rules —
   none of these surface at merge time. Conformance is a property of
   the *current* code-spec pair, not a historical artefact of past
   approvals.

5. **A load-bearing invariant is silently contradicted.** Five
   invariant categories: architectural decisions, data-structure
   choices, explicit constraints, non-functional requirements, and
   out-of-scope items. A change that contradicts any such invariant —
   in code or in a sibling spec — must surface, never slip. *Not* a
   hard reject — clashes require human judgement (see Lanes, below).

## Dimensions

The gate evaluates code on three dimensions, all together. Failure on
any one is a flag.

- **Conformance** — for every claim in the spec, there is a true code
  path that makes it real.
- **Style** — the implementation follows the consumer's code-style
  rules (conventionally consolidated in a style-rules document such as
  `docs/style-rules.md`, organised by language- or domain-specific
  family).
- **Test quality** — the tests follow the consumer's test-quality
  rules (typically in the same document); verifiers actually verify
  what they claim.

The specific rule families, their prefixes, and the path of the
style-rules document are consumer-defined. The gate evaluates against
whatever rules the consumer specifies; it does not impose a
particular taxonomy.

These three dimensions are not separable concerns; they are aspects
of the same binary question: *is the code good enough to ship?* They
live in one gate by design — fragmenting them produced the failure
pattern this spec exists to prevent.

## Lanes

The gate has two response paths. The choice is dictated by the kind of
failure detected, not by stage or scope.

- **Hard fail (rule violation).** Code breaks an entry in the
  consumer's style-rules document, or a `[verify]` test that asserts
  a specific behavioural claim returns failure. There is no legitimate
  "keep this on top" path. Gate refuses merge; recovery is *fix the
  code*.

- **Clarify (invariant clash).** Code (or a proposed spec change)
  contradicts a load-bearing invariant in a spec — one of the five
  categories from Invariant 5 above. The right path requires human
  judgement, framed by the *three-paths principle*:

  1. **Preserve the invariant** — rework the change so the invariant
     still holds.
  2. **Keep the change on top of the invariant** inelegantly, with the
     debt recorded in the spec or notes.
  3. **Change the invariant** — update the spec and plan follow-up
     work to realign code.

  The three-paths principle is *guidance, not a rigid template*. A
  given clash may need fewer or differently-framed options, each
  phrased in terms concrete to the clash.

  Gate raises `loom:clarify` per the *Options Format Contract*
  (defined in [Options Format Contract](#options-format-contract)
  below) and waits for `loom msg` resolution.

## Commands

The gate splits into two CLI commands by *kind* of check:

| Command | Kind | Purpose |
|---|---|---|
| **`loom check`** | Deterministic | Runs all mechanical audits — `[verify]` scripts, style linters (`cargo clippy`, `nix fmt`, `shellcheck`), the `--check=criteria` / `--check=removals` / `--check=infrastructure` / `--check=cross-spec` sub-audits, marker parsing, `bd-closed` lookup, diff shape. Cheap. Run frequently (pre-commit, on save, every CI commit). |
| **`loom review`** | LLM judge | Runs the LLM rubric — conformance trace, contract closure, verifier honesty, mock discipline, invariant-clash scan. Expensive. Run selectively (bead completion, on demand, scheduled). Consumes `loom check` results as input. |

Both commands take scope flags: `--bead <id>`, `--diff <range>`, or
`--tree`. The previous `loom doctor` command is removed; its
subchecks fold into `loom check --check=<name>`.

## Stages

Same gate, three points. Scope and cost-of-failure differ; the
underlying check is the same.

| Stage | Where | Scope | Cost-of-failure | Primary catches |
|---|---|---|---|---|
| **Plan** | `loom plan -n` / `loom plan -u` | Spec under interview | Lowest — no code yet | Missing claims, weak claims, missing verifier surfaces, invariant clashes in proposed spec changes |
| **Per-diff** | `loom check --bead <id>` then `loom review --bead <id>` | Spec sections the diff touches; the diff itself; tests in the diff | Medium — one bead's worth | Conformance gaps in diff, lint violations, weak verifiers, contract gaps inside one diff's reach, invariant clashes in proposed code changes |
| **Standing** | `loom check --tree` + `loom review --tree` (on-demand, CI, scheduled) | Entire spec tree × entire implementation | Highest — deployed code | Cross-file incoherence, contracts orphaned across PRs, omissions that never had an owner, accumulated style/test regressions, invariant clashes that slipped past prior reviews, template-vs-spec drift (Invariant 3) |

The plan stage has no separate command invocation — the agent runs
the rubric inline during the planning interview, and `loom plan` is
the surface that opens that interview. The other two stages compose
`loom check` and `loom review` as listed.

The standing stage is **non-optional**. Per Rothrock 2026, file-scoped
review catches 0% of cross-file incoherence; without project-scoped
review, the dominant cross-component failure mode is structurally
invisible.

### Plan-stage checks

The plan stage runs inside the planning interview — the agent's
rubric. Three checks must satisfy before the interview can commit:

1. **Completeness check.** Every requirement the user expressed has a
   checkable surface: a `[ ]` Success Criteria entry with `[verify]`
   or `[judge]`, a lifecycle / decision / contract table row, or an
   explicit `## Out of Scope` declaration. Implicit assumptions are
   surfaced; the agent either makes them testable or marks them
   non-testable with a reason.
2. **Internal coherence check.** The spec under interview is scanned
   for internal contradiction — two sections saying different things,
   decision-table rows that conflict, prose claims that can't both be
   true.
3. **Invariant-clash scan.** Check the anchor and any touched sibling
   specs for invariants the proposed change may contradict
   (architectural / data-structure / explicit-constraint /
   non-functional / out-of-scope). On detection, pause; resolve via
   three paths.

The agent doesn't separately *run* the gate at this stage — the gate
IS the agent's rubric. A check failing means the interview stays open
until the user resolves it.

(General agent discipline: at any stage, if the agent notices the
template it's running under contradicts the spec, it raises the
contradiction as a user question. This isn't a structured rubric item
at the plan stage — it's expected awareness. Mechanical detection of
template-vs-spec drift happens at the standing stage instead.)

### Per-diff stage checks

The per-diff stage composes `loom check` and `loom review` in
sequence.

**`loom check --bead <id>`** runs all deterministic audits. Marker
parsing, `bd-closed` lookup, diff non-empty / empty, every `[verify]`
script attached to the bead's criteria (none short-circuits another),
style linters (`cargo clippy -- -D warnings`, `nix fmt --check`,
`shellcheck`), and the mechanical `--check=*` sub-audits.

Any `loom check` failure routes into the existing hard-fail recovery
loop (`previous_failure` + `[loop] max_iterations`). Recovery doesn't
run `loom review` for the same iteration — mechanical failure is
sufficient grounds.

**`loom review --bead <id>`** runs the LLM rubric. Its inputs are:

- the diff
- the bead's intent (title, description, success criteria)
- the *full* touched spec sections (not just `## Affected Files`)
- the diffs and criteria of sibling beads bonded to the same molecule
- `[verify]` script sources
- `[judge]` rubric texts
- `loom check` results from the immediately-prior run

Rubric checks:

| Check | Dimension | Lane | Flag cause |
|---|---|---|---|
| **Conformance trace** — for every claim in touched spec sections, find a true code path (verifier-pass *or* LLM trace through current code) | Conformance | Hard fail | `spec-coherence-fail: <claim>` |
| **Contract closure** — for every multi-component contract the diff touches, verify completion in this diff or in bonded sibling diffs | Conformance | Hard fail | `orphan-integration: <contract>` |
| **Verifier honesty** — each `[verify]` the diff adds/modifies must support the claim it cites (no tautology, no mocking the thing under test) | Test quality | Hard fail | `tautological-assert: <test>` / `dishonest-verifier: <test>` |
| **Mock discipline** — mocks have a discernible reason; mock isn't the thing under test | Test quality | Hard fail | `mock-discipline: <test>` |
| **Cross-component verifier sufficiency** — multi-component contracts need a verifier that exercises the seam, not one side | Test quality | Hard fail | `verifier-too-narrow: <criterion>` |
| **Concurrency coverage** — production code introducing or modifying `Mutex`/`RwLock`/`Arc<Mutex<T>>` etc. needs at least one concurrent-load test | Test quality | Hard fail | `concurrency-untested: <lock-site>` |
| **Scope appropriateness** — diff matches bead's intent and stays close to `## Affected Files` | Conformance | Hard fail | `scope-creep` / `scope-shortfall` |
| **`[judge]` rubrics** — work satisfies each LLM-judgement criterion on the bead | Conformance | Hard fail | `judge-flag: <criterion>` |
| **Invariant clash** — for each touched spec section (anchor + sibling), scan for load-bearing invariants the diff contradicts | (cross-cutting) | **Clarify** (three paths) | `invariant-clash: <invariant>` |

Verdict: any hard-fail flag → recovery loop. Invariant-clash flag →
`loom:clarify` on the flagged bead. The clarified bead is skipped by
`bd ready` on subsequent ticks; non-dependent beads in the molecule
continue running. Push is held until the clarify is resolved via
`loom msg` (see push-gate semantics in
[loom-harness.md](loom-harness.md#functional)).

### Standing-stage checks

`loom check --tree` and `loom review --tree` run independently;
mechanical-only is fast and frequent, full sweep is rarer.

`loom check --tree` exercises every audit at tree scope: all
`[verify]` scripts, all linters, all `--check=*` sub-audits walking
every spec and every implementation file.

`loom review --tree` runs the LLM rubric against the whole spec set
× implementation. The checks from the per-diff rubric apply, scoped
to the tree rather than a diff. Additional standing-only check:

- **Template-vs-spec drift** (Invariant 3 enforcement). Reads every
  template loom uses (embedded in the loom binary, plus any
  consumer-provided overrides) against every spec in the consumer's
  spec tree. Flags any template instruction that contradicts a spec
  claim. Hard fail conceptually, but surfaced as a `bd` issue (no
  "merge to refuse" at standing stage).

Standing-stage flags become `bd` issues bonded to the relevant spec
section. Invariant clashes surfaced at standing stage raise
`loom:clarify`.

## Mechanisms

How conformance / style / test-quality are evaluated:

- **Verifier path.** A passing `[verify](path::fn)` test exercises the
  claim. Deterministic, mechanical. The gate trusts the verifier
  *only if* the test-quality dimension confirms the verifier is honest
  (Invariant 2).

- **Trace path.** An LLM trace through the consumer's current code
  finds the claim's implementation. Used when no verifier exists, or
  when the claim doesn't reduce to a single test (e.g., architectural
  invariants like *"loom never invokes `podman run` directly"*).

If both paths are available, both run. Failure on either → flag.

`## Affected Files` entries in specs are *planning scaffolding*, not
review-rubric surfaces. Whether `New` / `Modified` / `Removed` paths
are in the spec'd state is checked mechanically by
`loom check --check=criteria`, separately from the LLM rubric. The
rubric cares about *what the code does*, not *which files the diff
touches*.

## Options Format Contract

When the gate raises `loom:clarify` for an invariant clash, the bead
body presents the three (or contextual) paths as a structured
markdown block that `loom msg` can consume mechanically:

```markdown
## Options — <one-line summary of the clash>

### Option 1 — Preserve the invariant
<body explaining what reworking the change to preserve the invariant
would look like, including the cost>

### Option 2 — Keep the change on top of the invariant
<body explaining what carrying the contradiction would entail —
which spec section to record the debt in, what cleanup follow-up
to file>

### Option 3 — Change the invariant
<body explaining what updating the spec would entail — which
invariant to weaken or remove, what code realignment would follow>
```

`loom msg` consumes this format three ways:

- **List mode** (`loom msg`): the `## Options — <summary>` line is
  rendered as the bead's SUMMARY column.
- **View mode** (`loom msg -n <N>` / `loom msg -b <id>`): the full
  block is rendered to the user with each `### Option N` heading.
- **Fast-reply** (`loom msg -n <N> -o <K>`): the body of `### Option
  K` is recorded as the resolution note, the bead is closed, and
  `loom:clarify` is removed.

A clarify bead can present fewer or differently-framed options when
the clash warrants — the format is `### Option <integer> — <title>`
for any integer ≥ 1. The summary line is always required.

## Output

The gate's output is a verdict (pass / hard-fail / clarify) plus any
flagged actions. There is no separate persistence layer — `bd` issues
and git commits already provide the durable record:

- Hard-fail flags drive the existing recovery loop with
  `previous_failure` context.
- Invariant-clash flags raise `loom:clarify` beads per the Options
  Format Contract.
- Standing-stage flags create `bd` issues bonded to the relevant
  spec section.

Past gate runs are not persisted; *past passes don't grant immunity
from re-evaluation*. Conformance is a property of the current
code-spec pair, not a historical fact. Observability of gate
behaviour over time, if needed, is added separately and is not part
of this spec.

## Recovery

Per-stage flag handling:

- **Plan** — interview held until the spec is amended (claim
  surfaced, clash resolved, or explicitly out-of-scope'd). User
  authorisation required to ship a spec with unresolved gaps.
- **Per-diff** — hard-fail flags enter the existing recovery loop
  (`[loop] max_iterations`, default 3) with `previous_failure`
  rendered into the next prompt. Iterations 1–2 are same-agent
  retries with cumulative `previous_failure`. Iteration 3 (the final
  attempt) uses a **fresh agent**: new container, new agent process,
  blank scratchpad; receives the spec, the bead's criteria, the
  cumulative `previous_failure`, and the current state of the
  worktree — but *not* the prior session's transcript or in-memory
  context. Rationale: Rothrock 2026 (see *Empirical grounding*
  below) measured same-agent retry at 31.5% recovery rate (54.8%
  re-fail); the final attempt gets failure evidence without the
  failed approach. Invariant clashes raise `loom:clarify` on the
  flagged bead; that bead is skipped by `bd ready` on subsequent
  ticks while non-dependent beads in the molecule continue running.
  `loom msg` resolves the clarify. Clashes never trigger fresh-agent
  retry.
- **Standing** — a `bd` issue is created per gap, bonded to the
  relevant spec section. Tracked separately from in-flight molecules;
  picked up by any future `loom todo` run. Invariant clashes surface
  via `loom:clarify` in the next `loom msg` walk.

## Not in scope of this spec

The gate enforces; it does not own:

- The *content* of the consumer's style-rules document — which rules
  exist, how they're organised, what prefixes the consumer uses. The
  gate references the rules; the rules are authored by each consumer.
- The *content* of `[verify]` / `[judge]` tests. The gate runs them;
  they live in `tests/`.
- Workflow events (push, merge, bead lifecycle, fix-up bonding,
  molecule progress). Those are downstream of the gate's verdict, not
  properties the gate evaluates.
- The `loom:clarify` resolution channel itself — `loom msg` is the
  surface, defined in [loom-harness.md](loom-harness.md) Msg Modes.

## Empirical grounding

Rothrock 2026, *Gate Analysis* (5,109 gate checks across a 97-day
autonomous-development study), provides the empirical backbone for
several design choices:

- **Omissions dominate** — ~49% of rejected work is omission, ~4× more
  common than incoherence (13%) and more common than systematic errors
  (38%). The gate must check completeness explicitly, not just
  coherence. → Invariant 1 (multi-component contracts), Standing stage.
- **File-scoped review is blind to cross-file errors** — 0% incoherence
  detection. → Standing stage is non-optional; per-diff alone is
  insufficient.
- **Plan-stage has highest rejection rate (~61%) and highest ROI** —
  errors caught before code exists are cheapest. → Plan stage is a
  first-class gate, not implicit.
- **Revision-cycle is the weakest link** — only 31.5% of rejected work
  passes on retry; 54.8% re-fails. → Recovery bounds iterations at
  `[loop] max_iterations` (default 3); iteration 3 is a fresh agent
  rather than same-agent retry.
- **Trust ledger framing** — gate runs are trust evidence, not
  alignment proofs. → Framing adopted; persistence implication
  rejected. `bd` issues and git commits are the durable record.

## Decisions Log

All design questions raised during the initial design of this spec
are resolved. Kept here for traceability so a future reader can see
why each piece of the gate is shaped the way it is.

1. ~~**Invariant recognition in spec prose.**~~ Resolved: LLM
   judgement, anchored by conventions (`## Out of Scope` and
   `## Non-Functional` sections, imperative-keyword sentences,
   architectural claims like *"X never calls Y"*, schema /
   data-structure declarations). The LLM uses anchors as a strong
   starting point and catches prose-only invariants in addition.
2. ~~**Honest-verifier mechanism.**~~ Resolved: four-sub-check refactor
   of the verifier-honesty rubric — `verifier-bypass`,
   `fabricated-result`, `weak-assertion`, `coincidental-pass`. A test
   is honest iff it satisfies all four. Standing stage re-checks
   existing `[verify]`s against current spec/code to detect drift.
3. ~~**Recovery semantics, fresh-agent retry.**~~ Resolved: per
   iteration, not per cause. Iterations 1–2 are same-agent with
   cumulative `previous_failure`. Iteration 3 (final attempt) is a
   fresh agent that gets failure evidence but not prior-session
   transcript / scratchpad.
4. ~~**Interaction with `loom-harness.md` Verdict Gate.**~~ Resolved
   and executed during initial design: this spec owns the rubric
   and the gate-as-concept; `loom-harness.md` owns the execution
   layer
   (decision table, recovery mechanics, markers, labels,
   infra-failure handling). The seven-Concerns expansion and the
   Molecule-Level Closure Gate section have been removed from
   `loom-harness.md`. Functional command set updated: `loom check`
   subsumes the former `loom doctor`; `loom review` added.
5. ~~**Style-rules references to `loom doctor`.**~~ Resolved:
   `loom doctor --check=*` → `loom check --check=*` globally swept
   in the consumer's style-rules document. Test-quality rule content
   unchanged; only the command name in references was updated.
6. **Pre-commit hook updates.** *Deferred to implementation.* The
   `.pre-commit-config.yaml` hook currently calls `loom doctor
   --check=criteria`. The spec calls for the `--check=<name>` flags
   to move from `loom doctor` to `loom check` (and for the `doctor.rs`
   module to be renamed to `check.rs`). The hook config edit needs to
   land **after** the binary surface rename, not before — otherwise
   the hook breaks on the existing binary. Tracked as part of the
   broader `loom check / loom review` implementation work that will be
   decomposed by `loom todo`.
7. ~~**Naming.**~~ Resolved: spec named `loom-gate.md`, titled "Loom
   Gate." The LLM command remains `loom review`. The deterministic
   command remains `loom check`. The umbrella concept is "the gate"
   (or "the Loom Gate"), separate from the *Verdict Gate* execution
   layer in `loom-harness.md` and from the *push gate*.
