## Live-Path Coverage and Mock Discipline

The verdict-gate review's primary concern is **live-path coverage**, governed
by two rules:

**Rule 1 — Coverage.** At least one `[verify]` test on the bead must
exercise the live path: same binary, same argv shape, same env as the
real invocation. The bead's full `[verify]` set being entirely mocks is
a flag — *somewhere* in the set, the live path has to run.

**Rule 2 — Mock discipline.** Mocks are not forbidden. Each mock needs a
discernible reason: cost, flakiness, isolating an orthogonal concern,
driving a hard-to-trigger error path. A mock standing in for the very
thing the test claims to test is a flag.

**Acceptable mocks (no flag):**

- Mocking the LLM API in a retry-behaviour test — real calls are slow
  and flaky, and the test's concern is the retry logic.
- Mocking the filesystem when the test's actual concern is argument
  parsing or config resolution.
- Mocking a third-party service to drive an error path that's hard to
  trigger live.

**Flagged mocks and dead tests:**

- A bead's full `[verify]` set is entirely mocks — no test in the set
  exercises the live path end-to-end.
- Mocking the agent backend in a test that claims to test agent
  integration.
- A test whose fixture diverges from the live invocation derivation
  (different env vars, different argv, different working directory).
- Asserting `result/bin/loom` exists instead of *running* the binary
  at that path.
- A `cargo build` / `cargo check` standing in for a behaviour test on a
  module the diff never imports — green build, dead code path.
- A test ending with `|| true`, silent `2>/dev/null`, or any swallowed
  exit code that lets the script return 0 regardless of the real
  outcome.

## Style-Rule Conformance

The diff must satisfy every applicable rule in `{{ style_rules }}`. This
is the load-bearing defense for any rule that linters cannot mechanically
enforce — most rules in the document are prose, and the LLM judge is what
enforces them. *"Style looks fine"* is not an acceptable answer; the
output must enumerate which rules were checked.

**How to walk the document.** Open `{{ style_rules }}` and walk every
rule family in order, rule by rule:

- **SH-** (Shell)
- **NX-** (Nix)
- **DOC-** (Documentation)
- **GIT-** (Git)
- **TST-** (Testing)
- **RS-** (Rust)
- **COM-** (Comments)
- **CLI-** (CLI surface)

For each rule, judge whether the diff satisfies it. A rule that does not
apply to this diff (e.g. `SH-1` against a pure-Rust diff) is *checked
and dismissed*, not skipped silently — say so in the output.

**Citation contract.** For every violation you identify, the output
**must** cite both:

- the **rule id** — e.g. `RS-12`, `COM-1`, `SH-6`
- the **offending file and line range** — e.g.
  `loom/crates/loom-driver/src/agent/parser.rs:142-156`

One violation per bullet; never aggregate multiple rules into one
citation. A finding without a rule id is not actionable; a finding
without a file/line range is not auditable.

**Flag emission.** Any style-rule violation is a hard fail. Emit a
single `LOOM_REVIEW_FLAG: style-rule -- <summary>` line per the Flag
Emission Schema below. The summary should name the most load-bearing
violation by rule id; the per-violation citations above carry the full
list in the visible body of your response.
