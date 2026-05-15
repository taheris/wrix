# Loom Gate

The quality gate. Decides whether code is good enough to ship.

The umbrella concept covering all three stages (plan / per-diff /
standing) and both commands (`loom check` deterministic, `loom review`
LLM-judged). Distinct from the *Verdict Gate* execution layer in
[loom-harness.md](loom-harness.md) — that section owns the per-bead
mechanics that wrap the gate; this spec owns the rubric, the
invariants, the lanes, and the stages.

## Problem Statement

Loom's review machinery has multiple participants: a verdict gate
in [loom-harness.md](loom-harness.md) (per-bead, per-diff,
narrowly scoped), style rules in
[`docs/style-rules.md`](../docs/style-rules.md) (mechanical
lints), test strategy in [loom-tests.md](loom-tests.md). Each
carries part of the load. The gaps *between* them — cross-file
incoherence, multi-component contracts no individual bead owns,
omissions where no PR is the natural owner of the integration —
are structurally invisible without a consolidated review surface.

Omissions are the dominant failure mode in autonomous development —
more common than incoherence, more common than systematic errors.
File-scoped review detects none of the cross-file incoherence.
Coherence-only or file-scoped gates structurally cannot catch the
dominant failure modes.

This spec gives one place one responsibility: catch divergences
before they ship.

## Invariants — what must never happen

The five failure classes the gate guarantees against. These are the
gate's reason for existing; everything below them is mechanism.

1. **A spec claim is false in the code.** If a spec says X must
   happen, the implementation must make X happen. If a spec bans
   Y, the implementation must not contain Y. Includes
   multi-component contracts: parts {a, b, c} of a lifecycle
   either all land in the implementation, or the unfinished parts
   have a bonded successor doing the remaining work.

2. **A passing verifier is dishonest.** A `[verify]` test that asserts
   a tautology, mocks the thing it claims to test, or passes for the
   wrong reason is itself a divergence — the spec claim it cites is in
   fact unchecked. The gate distinguishes honest from dishonest
   verifiers; *all tests pass* is not synonymous with *the spec is
   enforced*.

3. **A template directs agents toward spec-contradicting behaviour.**
   Planning, decomposition, and review templates are themselves
   system artefacts. They must operate consistently with the specs
   they drive — a template whose instructions contradict its spec
   produces cascading damage as the agent follows the template
   literally instead of the contract.

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
| **`loom check`** | Deterministic | Runs all mechanical audits — `[verify]` scripts, style linters (`cargo clippy`, `nix fmt`, `shellcheck`), the `criteria` and `surface` sub-audits (each runnable individually as `loom check <audit>`), marker parsing, `bd-closed` lookup, diff shape. Cheap. Run frequently (pre-commit, on save, every CI commit). |
| **`loom review`** | LLM judge | Runs the LLM rubric — conformance trace, contract closure, verifier honesty, mock discipline, invariant-clash scan. Expensive. Run selectively (bead completion, on demand, scheduled). Consumes `loom check` results as input. |

Both commands take scope flags: `--bead <id>`, `--diff <range>`, or
`--tree`.

## Stages

Same gate, three points. Scope and cost-of-failure differ; the
underlying check is the same.

| Stage | Where | Scope | Cost-of-failure | Primary catches |
|---|---|---|---|---|
| **Plan** | `loom plan -n` / `loom plan -u` | Spec under interview | Lowest — no code yet | Missing claims, weak claims, missing verifier surfaces, invariant clashes in proposed spec changes |
| **Per-diff** | `loom check --bead <id>` then `loom review --bead <id>` | Spec sections the diff touches; the diff itself; tests in the diff | Medium — one bead's worth | Conformance gaps in diff, lint violations, weak verifiers, contract gaps inside one diff's reach, invariant clashes in proposed code changes |
| **Standing** | `loom check --tree` + `loom review --tree` (on-demand, CI, scheduled, **and unconditionally on `loom run` molecule completion — see [loom-harness.md FR1 + FR9](loom-harness.md#functional)**) | Entire spec tree × entire implementation | Highest — deployed code, **blocks push** when triggered by molecule completion | Cross-file incoherence, contracts orphaned across PRs, omissions that never had an owner, accumulated style/test regressions, invariant clashes that slipped past prior reviews, template-vs-spec drift (Invariant 3), surface drift (commands/flags spec lists but binary lacks, or vice-versa) |

The plan stage has no separate command invocation — the agent runs
the rubric inline during the planning interview, and `loom plan` is
the surface that opens that interview. The other two stages compose
`loom check` and `loom review` as listed.

The standing stage is **non-optional**. File-scoped review catches
0% of cross-file incoherence; without project-scoped review, the
dominant cross-component failure mode is structurally invisible.

### Plan-stage checks

The plan stage is first-class: errors caught before code exists
are cheapest. The stage runs inside the planning interview — the
agent's rubric. Three checks must satisfy before the interview can
commit:

1. **Completeness check.** Every requirement the user expressed has a
   checkable surface: a Success Criteria bullet with a `[verify]` or
   `[judge]` annotation, a lifecycle / decision / contract table row,
   or an explicit `## Out of Scope` declaration. Implicit assumptions
   are surfaced; the agent either makes them testable or marks them
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
`shellcheck`), and the mechanical `loom check <audit>` sub-audits.

Any `loom check` failure routes into the existing hard-fail recovery
loop (`previous_failure` + `[loop] max_iterations`). Recovery doesn't
run `loom review` for the same iteration — mechanical failure is
sufficient grounds.

**`loom review --bead <id>`** runs the LLM rubric. Its inputs are:

- the diff
- the bead's intent (title, description, success criteria)
- the *full* touched spec sections (not only the bullets the diff lines map to)
- the diffs and criteria of sibling beads bonded to the same molecule
- `[verify]` script sources
- `[judge]` rubric texts
- `loom check` results from the immediately-prior run

Rubric checks:

| Check | Dimension | Lane | Flag cause |
|---|---|---|---|
| **Conformance trace** — for every claim in touched spec sections, find a true code path (verifier-pass *or* LLM trace through current code). Scope includes the *full* touched spec sections — command-set tables, interface specs, decision tables, prose constraints — not only the bullets a diff line maps to. | Conformance | Hard fail | `spec-coherence-fail: <claim>` |
| **Contract closure** — for every multi-component contract the diff touches, verify completion in this diff or in bonded sibling diffs | Conformance | Hard fail | `orphan-integration: <contract>` |
| **Style-rule conformance** — diff complies with every rule in the consumer's pinned `{{ style_rules }}` document that linters cannot enforce mechanically. The judge discovers rule families from the document itself (no fixed prefix list — adapts to whatever convention the consuming project uses) and cites the rule id + file/line for each violation. | Style | Hard fail | `style-rule-violation: <rule-id>` |
| **Verifier honesty** — each `[verify]` the diff adds/modifies must support the claim it cites. Decomposed into four sub-checks; a test is honest iff it satisfies all four. (a) **verifier-bypass** — does the verifier actually exercise the live path? (b) **fabricated-result** — does the verifier's pass rely on a value the test itself synthesized? (c) **weak-assertion** — does the assertion meaningfully constrain the result, or does it tautologically pass? (d) **coincidental-pass** — does the test pass for the right reason, or because of an unrelated property of the system? Standing stage re-checks existing `[verify]`s against current spec/code to detect drift. | Test quality | Hard fail | `verifier-bypass: <test>` / `fabricated-result: <test>` / `weak-assertion: <test>` / `coincidental-pass: <test>` |
| **Mock discipline** — mocks have a discernible reason; mock isn't the thing under test | Test quality | Hard fail | `mock-discipline: <test>` |
| **Cross-component verifier sufficiency** — multi-component contracts need a verifier that exercises the seam, not one side | Test quality | Hard fail | `verifier-too-narrow: <criterion>` |
| **Concurrency coverage** — production code introducing or modifying `Mutex`/`RwLock`/`Arc<Mutex<T>>` etc. needs at least one concurrent-load test | Test quality | Hard fail | `concurrency-untested: <lock-site>` |
| **Scope appropriateness** — diff matches the bead's stated intent (title, description, success criteria) and the spec sections it claims to implement; touching files unrelated to that intent is creep, missing files necessary to satisfy the criteria is shortfall | Conformance | Hard fail | `scope-creep` / `scope-shortfall` |
| **`[judge]` rubrics** — work satisfies each LLM-judgement criterion on the bead | Conformance | Hard fail | `judge-flag: <criterion>` |
| **Invariant clash** — for each touched spec section (anchor + sibling), scan for load-bearing invariants the diff contradicts. The judge uses spec conventions as anchors (`## Out of Scope` and `## Non-Functional` sections, imperative-keyword sentences, architectural claims like *"X never calls Y"*, schema / data-structure declarations) and also catches prose-only invariants that lack a structural anchor. | (cross-cutting) | **Clarify** (three paths) | `invariant-clash: <invariant>` |

The style-rule-conformance check is the load-bearing defense for any
rule that cannot be expressed as a clippy / lint pattern. Most rules
in a typical `style-rules.md` are prose; the LLM judge is what
enforces them. The rubric requires walking the document concretely —
for every rule discovered in the pinned `{{ style_rules }}` document,
the judge either finds the supporting code or flags the violation
with the rule id. "Style looks fine" is not an acceptable answer; the
output must enumerate which rules were checked. The rule-family
prefixes vary per consuming project; the judge must adapt to whatever
the document uses rather than expecting a fixed set.

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
`[verify]` scripts, all linters, all `loom check <audit>` sub-audits walking
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

### Surface-conformance audit (`loom check surface`)

A deterministic sub-audit (no LLM call) that diffs the consumer's
spec-declared user-facing surface against the compiled binary.
Closes the class of failure where the spec mandates a command or
flag the binary never grew (or fails to remove one the spec marked
removed). See [loom-harness.md FR13](loom-harness.md#functional)
for the four hard-fail dimensions and audit triggers.

**Boundary with `loom review`'s style-rule walk.** Help-text wording
is **not** a surface-audit dimension. CLI-style requirements (e.g. a
short single-sentence help line, no implementation references) live
under the LLM-judged style-rule walk so spec prose can be polished
without churning a deterministic gate. The surface audit checks that
commands and flags exist with the right names and grouping —
nothing about how they describe themselves.

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
  bounded by `[loop] max_iterations` with `previous_failure`
  rendered into the next prompt. All iterations except the last
  are same-agent retries with cumulative `previous_failure`; the
  final iteration uses a **fresh agent**: new container, new agent
  process, blank scratchpad; receives the spec, the bead's
  criteria, the cumulative `previous_failure`, and the current
  state of the worktree — but *not* the prior session's transcript
  or in-memory context. Rationale: same-agent retry has a low
  recovery rate and a high re-fail rate; the final attempt gets
  failure evidence without the failed approach. Invariant clashes
  raise `loom:clarify` on the flagged bead; that bead is skipped
  by `bd ready` on subsequent ticks while non-dependent beads in
  the molecule continue running. `loom msg` resolves the clarify.
  Clashes never trigger fresh-agent retry.
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
