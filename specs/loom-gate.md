# Loom Gate

The quality gate. Decides whether code is good enough to ship.

The umbrella concept covering all three stages (plan / per-diff /
standing) and one command tree (`loom gate <subcommand>`).
`loom gate verify` runs deterministic verifiers; `loom gate review`
runs the LLM rubric. Distinct from the *Verdict Gate* execution
layer in [loom-harness.md](loom-harness.md) — that section owns the
per-bead mechanics that wrap the gate; this spec owns the rubric,
the invariants, the lanes, and the stages.

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

2. **A passing verifier is dishonest.** A deterministic verifier
   (`[check]`, `[test]`, or `[system]`) that asserts a tautology,
   mocks the thing it claims to test, or passes for the wrong reason
   is itself a divergence — the spec claim it cites is in fact
   unchecked. The gate distinguishes honest from dishonest
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
  consumer's style-rules document, or a deterministic verifier
  (`[check]`, `[test]`, or `[system]`) that asserts a specific
  behavioural claim returns failure. There is no legitimate
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

The gate is one umbrella command, `loom gate`, with subcommands
selecting the audit scope:

| Command | Kind | Purpose |
|---|---|---|
| **`loom gate`** | Status | Reads cached results from the last `verify` / `review` / `audit` run (sqlite-backed) and prints a fast status report — no verifiers run. See *Status cache* for the hard latency target. |
| **`loom gate audit`** | All | Runs everything — `verify` then `review`. The full PR-gate path. |
| **`loom gate verify`** | Deterministic | Runs every `[check]` / `[test]` / `[system]` verifier. Cheap relative to review; expensive relative to status. Run frequently (pre-commit, on save, every CI commit). |
| **`loom gate check`** | Deterministic, one tier | Runs only `[check]`-tier verifiers (static analysis of source). Fastest tier; suitable for tight feedback loops. |
| **`loom gate test`** | Deterministic, one tier | Runs only `[test]`-tier verifiers, batched into one runner subprocess per invocation. |
| **`loom gate system`** | Deterministic, one tier | Runs only `[system]`-tier verifiers (containers, packaging, end-to-end). Slow; CI-only by default. |
| **`loom gate review`** | LLM judge | Runs the LLM rubric — conformance trace, contract closure, verifier honesty, mock discipline, invariant-clash scan. Expensive. Run selectively (bead completion, on demand, scheduled). Consumes `verify` results as input. Includes `[judge]`-tier criterion verifiers and the rubric walk over the diff. |
| **`loom gate judge`** | LLM judge, one lane | Runs only criterion-attached `[judge]` verifiers — skips the rubric walk. Useful when only one lane needs re-running. |
| **`loom gate rubric`** | LLM judge, one lane | Runs only the rubric walk over the diff — skips criterion-attached judges. |

All subcommands take scope flags: `--bead <id>`, `--diff <range>`,
or `--tree`. The batched and per-tier subcommands additionally
accept:

- `--files <path>...` — filter to verifiers whose scope intersects
  the file set (used by pre-commit hooks for fast feedback)
- `--spec <label>` — filter to one spec's criteria
- `<selector>` (positional) — run one specific verifier by its
  annotation target

The composition: `loom gate audit` ≡ `loom gate verify && loom gate
review`. For lane subsets without a named alias (e.g. "all
deterministic without `system`"), shell composition is the path:
`loom gate check && loom gate test`.

## Stages

Same gate, three points. Scope and cost-of-failure differ; the
underlying check is the same.

| Stage | Where | Scope | Cost-of-failure | Primary catches |
|---|---|---|---|---|
| **Plan** | `loom plan -n` / `loom plan -u` | Spec under interview | Lowest — no code yet | Missing claims, weak claims, missing verifier surfaces, invariant clashes in proposed spec changes |
| **Per-diff** | `loom gate verify --bead <id>` then `loom gate review --bead <id>` (or `loom gate audit --bead <id>` for both) | Spec sections the diff touches; the diff itself; tests in the diff | Medium — one bead's worth | Conformance gaps in diff, lint violations, weak verifiers, contract gaps inside one diff's reach, invariant clashes in proposed code changes |
| **Standing** | `loom gate audit --tree` (on-demand, CI, scheduled, **and unconditionally on `loom run` molecule completion — see [loom-harness.md FR1 + FR9](loom-harness.md#functional)**) | Entire spec tree × entire implementation | Highest — deployed code, **blocks push** when triggered by molecule completion | Cross-file incoherence, contracts orphaned across PRs, omissions that never had an owner, accumulated style/test regressions, invariant clashes that slipped past prior reviews, template-vs-spec drift (Invariant 3), surface drift (commands/flags spec lists but binary lacks, or vice-versa) |

The plan stage has no separate command invocation — the agent runs
the rubric inline during the planning interview, and `loom plan` is
the surface that opens that interview. The other two stages compose
`loom gate verify` and `loom gate review` (or invoke `loom gate
audit` for both) as listed.

The standing stage is **non-optional**. File-scoped review catches
0% of cross-file incoherence; without project-scoped review, the
dominant cross-component failure mode is structurally invisible.

### Plan-stage checks

The plan stage is first-class: errors caught before code exists
are cheapest. The stage runs inside the planning interview — the
agent's rubric. Three checks must satisfy before the interview can
commit:

1. **Completeness check.** Every requirement the user expressed has a
   checkable surface: a Success Criteria bullet with a `[check]`,
   `[test]`, `[system]`, or `[judge]` annotation, a lifecycle /
   decision / contract table row, or an explicit `## Out of Scope`
   declaration. Implicit assumptions are surfaced; the agent either
   makes them testable or marks them non-testable with a reason.
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

The per-diff stage composes `loom gate verify` and `loom gate review`
in sequence (or invokes `loom gate audit --bead <id>` for both).

**`loom gate verify --bead <id>`** runs all deterministic audits.
Marker parsing, `bd-closed` lookup, diff non-empty / empty, every
deterministic verifier (`[check]` / `[test]` / `[system]`) attached
to the bead's criteria (none short-circuits another), style linters
(`cargo clippy -- -D warnings`, `nix fmt --check`, `shellcheck`),
and any `[check]`-tier walks the consumer has registered for
cross-cutting structural audits.

Any `verify` failure routes into the existing hard-fail recovery
loop (`previous_failure` + `[loop] max_iterations`). Recovery doesn't
run `loom gate review` for the same iteration — mechanical failure
is sufficient grounds.

**`loom gate review --bead <id>`** runs the LLM rubric. Its inputs
are:

- the diff
- the bead's intent (title, description, success criteria)
- the *full* touched spec sections (not only the bullets the diff lines map to)
- the diffs and criteria of sibling beads bonded to the same molecule
- deterministic-verifier sources (`[check]` walk implementations, `[test]` function bodies, `[system]` scripts)
- `[judge]` rubric texts
- `loom gate verify` results from the immediately-prior run

Rubric checks:

| Check | Dimension | Lane | Flag cause |
|---|---|---|---|
| **Conformance trace** — for every claim in touched spec sections, find a true code path (verifier-pass *or* LLM trace through current code). Scope includes the *full* touched spec sections — command-set tables, interface specs, decision tables, prose constraints — not only the bullets a diff line maps to. | Conformance | Hard fail | `spec-coherence-fail: <claim>` |
| **Contract closure** — for every multi-component contract the diff touches, verify completion in this diff or in bonded sibling diffs | Conformance | Hard fail | `orphan-integration: <contract>` |
| **Style-rule conformance** — diff complies with every rule in the consumer's pinned `{{ style_rules }}` document that linters cannot enforce mechanically. The judge discovers rule families from the document itself (no fixed prefix list — adapts to whatever convention the consuming project uses) and cites the rule id + file/line for each violation. | Style | Hard fail | `style-rule-violation: <rule-id>` |
| **Verifier honesty** — each deterministic verifier the diff adds or modifies (`[check]`, `[test]`, `[system]`) must support the claim it cites. Decomposed into four sub-checks; a verifier is honest iff it satisfies all four. (a) **verifier-bypass** — does the verifier actually exercise the live path? (b) **fabricated-result** — does the verifier's pass rely on a value the test itself synthesized? (c) **weak-assertion** — does the assertion meaningfully constrain the result, or does it tautologically pass? (d) **coincidental-pass** — does the verifier pass for the right reason, or because of an unrelated property of the system? Standing stage re-checks existing verifiers against current spec/code to detect drift. | Test quality | Hard fail | `verifier-bypass: <verifier>` / `fabricated-result: <verifier>` / `weak-assertion: <verifier>` / `coincidental-pass: <verifier>` |
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

`loom gate verify --tree` and `loom gate review --tree` run
independently (or `loom gate audit --tree` for both); mechanical-only
is fast and frequent, full sweep is rarer.

`loom gate verify --tree` exercises every audit at tree scope: every
`[check]` / `[test]` / `[system]` verifier, all linters, all
`[check]`-tier walks the consumer has registered, walking every spec
and every implementation file.

`loom gate review --tree` runs the LLM rubric against the whole spec
set × implementation. The checks from the per-diff rubric apply,
scoped to the tree rather than a diff. Additional standing-only
check:

- **Template-vs-spec drift** (Invariant 3 enforcement). Reads every
  template loom uses (embedded in the loom binary, plus any
  consumer-provided overrides) against every spec in the consumer's
  spec tree. Flags any template instruction that contradicts a spec
  claim. Hard fail conceptually, but surfaced as a `bd` issue (no
  "merge to refuse" at standing stage).

Standing-stage flags become `bd` issues bonded to the relevant spec
section. Invariant clashes surfaced at standing stage raise
`loom:clarify`.

### Surface-conformance audit

A deterministic audit (no LLM call) that diffs the consumer's
spec-declared user-facing surface against the compiled binary.
Closes the class of failure where the spec mandates a command or
flag the binary never grew (or fails to remove one the spec marked
removed). Implemented as a `[check]`-tier verifier rather than a
separate subcommand: the consumer annotates the relevant spec
criterion with `[check](<command that diffs declared surface against
the binary>)`. See [loom-harness.md FR13](loom-harness.md#functional)
for the four hard-fail dimensions and audit triggers.

**Boundary with `loom gate review`'s style-rule walk.** Help-text
wording is **not** a surface-audit dimension. CLI-style requirements
(e.g. a short single-sentence help line, no implementation
references) live under the LLM-judged style-rule walk so spec prose
can be polished without churning a deterministic gate. The surface
audit checks that commands and flags exist with the right names and
grouping — nothing about how they describe themselves.

## Mechanisms

How conformance / style / test-quality are evaluated:

- **Verifier path.** A passing deterministic verifier (`[check]`,
  `[test]`, or `[system]`) exercises the claim. Deterministic,
  mechanical. The gate trusts the verifier *only if* the test-quality
  dimension confirms the verifier is honest (Invariant 2).

- **Trace path.** An LLM trace through the consumer's current code
  finds the claim's implementation. Used when no verifier exists, or
  when the claim doesn't reduce to a single test (e.g., architectural
  invariants like *"loom never invokes `podman run` directly"*).

If both paths are available, both run. Failure on either → flag.

## Annotation resolution

Each criterion's annotation is resolved per its tier:

| Tier | Target shape | Dispatch |
|------|--------------|----------|
| `[check]` | `[check](command)` — shell command | Each annotation invokes its own process (often a walk binary the consumer ships). |
| `[test]` | `[test](path)` — language-native test path (e.g. `crate::module::test_name`, `tests/test_foo.py::test_bar`) | The gate collects all `[test]` targets in a single `loom gate test` invocation and issues **one** runner subprocess (e.g. `cargo nextest run -E 'test(p1) \| test(p2) \| ...'`). One process per invocation, full internal parallelism. |
| `[system]` | `[system](command)` — shell command | Each annotation invokes its own process. System verifiers are inherently slow and self-contained; batching doesn't help. |
| `[judge]` | `[judge](path)` — file path or criterion id whose content is the LLM rubric | The gate collects all `[judge]` targets and issues concurrent LLM calls (API-level parallelism). |

### Runner discovery

For batched tiers (`[test]`, `[judge]`), the gate needs to know how
to invoke the consumer's test runner with a list of targets. Two
mechanisms, layered:

1. **Defaults via toolchain detection.** The gate detects the
   consumer's toolchain and applies a built-in runner template:
   - `Cargo.toml` at repo root → `cargo nextest run -E 'test({paths})'`
   - `pyproject.toml` at repo root → `pytest -k '{paths_or}'`
   - `go.mod` at repo root → `go test -run '{paths_alt}' ./...`
   - Built-in template applies the appropriate `{paths}` join syntax.
2. **Repo override via `.loom/config.toml`.** When the default
   doesn't fit (multi-language repos, custom runners, unusual
   workflows), the consumer declares per-tier runners explicitly:
   ```toml
   [runner]
   test = "cargo nextest run -E 'test({paths})'"
   judge = "loom-judge {paths}"
   ```
   `{paths}` is replaced by the joined target list at invocation time.

Zero config for the common case; one-file override for everything
else.

### Verifier-runner contract

Every verifier — whether `[check]` command, `[system]` command, or
the runner invoked by `[test]` / `[judge]` batching — is a
subprocess that conforms to:

- **Input:** env vars (`LOOM_FILES=<paths>` for `--files` runs,
  `LOOM_SPEC=<label>`, etc.) plus argv from the annotation's command
  string.
- **Output:** a JSON line on stdout matching the typed-verdict
  shape — `{"pass": bool, "evidence": "<message>"}`.
- **Exit code:** mirrors `pass` (0 for true, non-zero for false).

This works for any language. The contract is process-shaped, not
language-shaped.

### `--files` scope handling

For batched tiers, the gate filters annotations to those whose
scope intersects `--files` before issuing the batched invocation:

- `[test]`-tier scope = files in `crate(test)` ∪ files in
  `crate(test)`'s transitive dependencies (Rust; computed via
  `cargo metadata`). Other toolchains supply analogous mappings.
- For non-batched tiers (`[check]`, `[system]`), the gate passes
  `LOOM_FILES` as env and the verifier decides whether to filter.
  Most verifiers can be dumb (run the same way regardless); walks
  that benefit from scope filtering read the env var.

### Test-tier silent-zero-match

`cargo test -- some_name` and equivalents in other runners exit 0
silently when no test matches the filter. The gate sniffs known
runners (`cargo test`, `cargo nextest`, `pytest`) and post-processes
output to detect zero-match cases, failing the run with a clear
error. Consumers using unrecognised runners must ensure their
runner fails on zero-match.

## Integrity gate

The deterministic gate that verifies the annotations themselves
resolve. Runs as part of `loom gate check`. Two directions:

1. **Forward — every annotation's target is valid for its tier.**
   - `[check](cmd)` and `[system](cmd)`: the command's first token
     resolves on PATH or as a file in the repo (best-effort —
     dynamic commands may resolve only at runtime).
   - `[test](path)`: the path resolves to a `#[test]` /
     `#[tokio::test]` / proptest function (or language equivalent)
     in the consumer's workspace, via the consumer's toolchain
     metadata.
   - `[judge](path)`: the path resolves to a file on disk.

2. **Atomic acceptance — each criterion carries exactly one
   annotation.** Two annotations on one criterion is a flag
   (ambiguous pass/fail when one passes and the other fails).
   N→1 sharing is allowed (multiple criteria pointing at the same
   verifier).

Failure output: `<spec>:<line>: annotation [tier](<target>) — does
not resolve` or `<spec>:<line>: criterion carries N annotations,
expected 1`.

The integrity gate is itself a `[check]`-tier verifier (its own
spec criterion annotates back to its implementation), so every
`loom gate check` run includes a self-test of the gate's resolution
logic.

## Status cache

`loom gate` (no subcommand) reads from a sqlite-backed status cache
and prints a fast report. `loom gate verify`, `loom gate review`,
and the tier subcommands write to the cache as they run.

**Cache contents per criterion:**
- annotation target
- last-run timestamp and commit hash
- pass / fail / skipped (scope) verdict
- evidence string from the verifier's JSON output

**Cache schema** extends the existing state-db schema in
[loom-harness.md](loom-harness.md). One row per criterion, indexed
by `(spec_label, criterion_anchor)`.

**Report contents** when `loom gate` runs without subcommands:
- per-spec criterion counts: total, annotated, un-annotated
- last-run summary per tier: when, pass/fail counts, currently-failing criteria
- annotation health: broken annotations (target doesn't resolve),
  stale runs (cache older than N days)

**Hard target:** report renders in <500ms on a corpus of arbitrary
size. A self-test asserts this — the cache implementation, not the
corpus, is what determines the latency.

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
- The *content* of `[check]` / `[test]` / `[system]` / `[judge]`
  verifiers. The gate runs them; they live in the consumer's repo.
- The *organisation* of the consumer's verifiers — whether the
  `[check]`-tier walks live in a dedicated crate, are scattered
  across source crates, or are shell scripts is the consumer's
  choice. The gate dispatches whatever annotation says, however the
  consumer chooses to back it.
- Workflow events (push, merge, bead lifecycle, fix-up bonding,
  molecule progress). Those are downstream of the gate's verdict, not
  properties the gate evaluates.
- The `loom:clarify` resolution channel itself — `loom msg` is the
  surface, defined in [loom-harness.md](loom-harness.md) Msg Modes.
