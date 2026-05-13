# Style Guidelines

Rules the Judge enforces mechanically. Every rejection must cite a rule by ID.
Rules not listed here cannot be used to reject — flag unlisted concerns for
the Mayor via `bd human` instead.

## Shell (SH-)

- **SH-1** — Every script must start with `set -euo pipefail`
- **SH-2** — All variable expansions must be quoted: `"$var"`, not `$var`
  (exception: intentional word-splitting with a comment explaining why)
- **SH-3** — Use `[[ ]]` for conditionals, not `[ ]`
- **SH-4** — Use `$(command)` for substitution, not backticks
- **SH-5** — Functions use `local` for all variables except intentional exports
- **SH-6** — Never silently suppress errors with `2>/dev/null || true` without
  a comment explaining which specific error is expected and why it's safe to
  ignore. Prefer precondition checks (`if [ -f ... ]; then`) over
  try-and-ignore. If an operation is genuinely best-effort (e.g. notifications),
  comment `# best-effort: <why>` on the same line. Treat `|| true` like an
  empty `catch {}` — it must justify what it's catching.

## Nix (NX-)

- **NX-1** — Use `inherit` to pull names from enclosing scope; do not repeat `x = x`
- **NX-2** — Files must pass `nix fmt` (nixfmt-rfc-style) with no diff
- **NX-3** — Keep derivations pure — no `builtins.fetchurl` without a hash,
  no `builtins.currentSystem`
- **NX-4** — Attrset arguments use `{ a, b, ... }:` destructuring, not `args: args.a`

## Documentation (DOC-)

- **DOC-1** — New specs go in `specs/` and must be added to the spec index
  in `docs/README.md` (the session-start pin)
- **DOC-2** — Architecture references point to `docs/architecture.md`, not `specs/`
- **DOC-3** — Terminology references point to `docs/README.md`

## Git (GIT-)

- **GIT-1** — Commit messages are imperative mood, max 72 chars for the subject line
- **GIT-2** — No secrets, credentials, or API keys in committed files
- **GIT-3** — Hidden specs (`.wrapix/ralph/state/`) must never be copied into `specs/`

## Testing (TST-)

- **TST-1** — Tests must execute the code under test, not grep for strings in source
- **TST-2** — Mock external dependencies (podman, network), not internal logic
- **TST-3** — Each test function tests one behavior and has a descriptive name
- **TST-4** — Any PR that updates an `insta` snapshot under `*/tests/snapshots/`
  must include a `snapshot updated because: <reason>` line in the PR
  description. Snapshots cover contract surfaces (Askama templates, CLI
  `--help`); silent diffs there are accidental drift, not a deliberate change.
- **TST-5** — Spec `[verify](tests/loom-test.sh::test_X)` annotations must
  point at a real `test_X()` dispatcher, and a checked criterion (`[x]`)
  must NOT route through `_pending_stub`. The pre-commit `loom-doctor`
  hook runs `loom doctor --check=criteria` on every change to `specs/`,
  `tests/loom-test.sh`, or `loom/crates/*/` and fails the commit on
  either violation. Reasoning: a `[x]` paired with a stub means the spec
  claims a guarantee the test layer doesn't enforce, which is exactly
  the drift the audit exists to surface.
