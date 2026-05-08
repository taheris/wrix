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
