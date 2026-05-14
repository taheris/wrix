# Style Rules

Rules the Judge enforces mechanically. Every rejection must cite a rule by ID.
Rules not listed here cannot be used to reject — flag unlisted concerns for
the Mayor via `bd human` instead.

## Shell (SH-)

- **SH-1** — *Every script must start with `set -euo pipefail`.*
- **SH-2** — *All variable expansions must be quoted.* `"$var"`, not
  `$var` (exception: intentional word-splitting with a comment
  explaining why).
- **SH-3** — *Use `[[ ]]` for conditionals, not `[ ]`.*
- **SH-4** — *Use `$(command)` for substitution, not backticks.*
- **SH-5** — *Functions use `local` for all variables except intentional
  exports.*
- **SH-6** — *Never silently suppress errors.* `2>/dev/null || true` is
  banned without a comment explaining which specific error is expected
  and why it's safe to ignore. Prefer precondition checks
  (`if [ -f ... ]; then`) over try-and-ignore. If an operation is
  genuinely best-effort (e.g. notifications), comment
  `# best-effort: <why>` on the same line. Treat `|| true` like an
  empty `catch {}` — it must justify what it's catching.

## Nix (NX-)

- **NX-1** — *Use `inherit` to pull names from enclosing scope; do not
  repeat `x = x`.* For `lib.*` specifically: if a Nix file calls
  `lib.elem`, `lib.optional`, or any `lib.X` more than once, lift the
  names with `inherit (lib) elem optional X;` at the top of the
  enclosing `let` block (or via destructuring on the module's argset)
  and use them bare. Single-use accessors can stay as `lib.X` inline.
  Repeated `lib.` prefixes are noise; the `inherit` form puts the
  imported surface up top where it's easy to audit.
- **NX-2** — *Files must pass `nix fmt` (nixfmt-rfc-style) with no diff.*
- **NX-3** — *Keep derivations pure.* No `builtins.fetchurl` without a
  hash, no `builtins.currentSystem`.
- **NX-4** — *Attrset arguments use `{ a, b, ... }:` destructuring,
  not `args: args.a`.*

## Documentation (DOC-)

- **DOC-1** — *New specs go in `specs/` and must be added to the spec
  index in `docs/README.md` (the session-start pin).*
- **DOC-2** — *Architecture references point to `docs/architecture.md`,
  not `specs/`.*
- **DOC-3** — *Terminology references point to `docs/README.md`.*

## Git (GIT-)

- **GIT-1** — *Commit messages are imperative mood, max 72 chars for the
  subject line.*
- **GIT-2** — *No secrets, credentials, or API keys in committed files.*
- **GIT-3** — *Hidden specs (`.wrapix/ralph/state/`) must never be
  copied into `specs/`.*

## Testing (TST-)

- **TST-1** — *Tests must execute the code under test, not grep for
  strings in source.*
- **TST-2** — *Mock external dependencies (podman, network), not
  internal logic.*
- **TST-3** — *Each test function tests one behavior and has a
  descriptive name.*
- **TST-4** — *Any PR that updates an `insta` snapshot under
  `*/tests/snapshots/` must include a `snapshot updated because:
  <reason>` line in the PR description.* Snapshots cover contract
  surfaces (Askama templates, CLI `--help`); silent diffs there are
  accidental drift, not a deliberate change.
- **TST-5** — *Every spec contract has a working verifier.* A contract
  is any spec element that asserts behaviour: a Success Criteria `[ ]`
  bullet, a row in a lifecycle / decision / contract table, an
  `## Affected Files` entry (New / Modified / Removed), or
  imperative-keyword prose (`MUST` / `MUST NOT` / `REQUIRES` /
  `CANNOT` / `NEVER` / `SHALL`). Each contract must carry — or live
  in a section that carries — a `[verify](path::fn)` or
  `[judge](path::fn)` annotation pointing at a real test. A checked
  criterion (`[x]`) must not route through `_pending_stub`.
  `loom check --check=criteria` enforces this across every contract
  surface and fails pre-commit on any of:
  - **Annotations:** target function does not exist; `[x]` routes
    through a stub
  - **Tables:** row count does not match matching verifier-entry count
    in the same section
  - **Affected Files:** New / Modified path absent on disk; Removed
    path still exists or is still referenced
  - **Normative prose:** section contains imperative-keyword
    sentences but carries no verifier annotation
- **TST-6** — *Orphan `[verify]` dispatchers are flagged.* Functions
  defined in `tests/loom-test.sh` that are referenced by no spec
  annotation are caught by `loom check --check=criteria`. Delete
  them, or wire a spec annotation that uses them.
- **TST-7** — *Removals demonstrate their replacements work.* When a
  change set deletes a test, code path, schema element, or file
  paired with a "replaces" / "replaced by" / "supersedes" claim
  (commit message, code comment, PR description, or
  `## Affected Files Removed` text), the replacement's verifier must
  land in the same change set. `loom check --check=removals` scans
  staged diffs and fails pre-commit on:
  - test-function or `[verify]` deletions paired with replacement-claim
    text where the change set adds no corresponding new test or
    `[verify]` annotation
  - schema migrations containing `ALTER TABLE … DROP COLUMN` or
    equivalent removal patterns where the change set still leaves
    source references to the removed element
- **TST-8** — *Test infrastructure is itself tested.* Stubs (stub
  agents, fixture helpers, `wrapix spawn` fakes, JSONL fakes) must
  carry at least one conformance test asserting the stub's observable
  contract matches the real implementation. `loom check` audit
  checks (`--check=criteria`, `--check=removals`, `--check=cross-spec`,
  etc.) must each carry a self-test with synthetic fixtures that
  exhibit the violation and assert the check fires. Without these,
  bugs in the test infrastructure or audit layer silently invalidate
  every downstream test. `loom check --check=infrastructure`
  enforces this.
- **TST-9** — *Cross-spec terms are consistent.* When a CLI flag,
  bead label, event variant, schema element, environment variable,
  or other named term appears in multiple specs in `specs/`, the
  meaning, type, and behaviour must match. `loom check --check=cross-spec`
  greps for shared terms across `specs/` and flags definitions that
  diverge.

## Rust (RS-)

### Workspace conventions (RS-1..RS-3)

- **RS-1** — *Edition 2024 + `resolver = "3"` at the workspace root.*
  Every crate inherits via `edition.workspace = true`,
  `version.workspace = true`, etc. No per-crate pins.
- **RS-2** — *Workspace-pinned dependencies.* Every third-party crate
  is pinned exactly once under `[workspace.dependencies]`. Member
  crates consume with `foo = { workspace = true }`.
- **RS-3** — *All lint configuration lives at the workspace root.* Two
  files own the surface.

  `Cargo.toml` → `[workspace.lints.rust]` + `[workspace.lints.clippy]`.
  Enable clippy groups (`all`, `pedantic`, `nursery`) at `warn` with
  `priority = -1`, then layer targeted overrides:
  - Opt in to restriction lints that enforce the production bans
    (`unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`,
    `unreachable`, `dbg_macro`, `print_stdout`, `print_stderr`).
  - Set `unsafe_code = "forbid"` under `[workspace.lints.rust]`.
  - `= "allow"` pedantic/nursery lints that don't earn their keep
    (e.g. `use_self`, `must_use_candidate`, `uninlined_format_args`,
    `significant_drop_tightening`, `too_many_lines`).

  Every member declares `[lints] workspace = true`. No crate-root
  `#![warn(...)]` / `#![deny(...)]`. CI runs
  `cargo clippy -- -D warnings`.

  `clippy.toml` (workspace root) sets the test exemptions clippy
  supports natively:
  ```toml
  allow-expect-in-tests = true
  allow-panic-in-tests  = true
  allow-unwrap-in-tests = true
  allow-print-in-tests  = true
  allow-dbg-in-tests    = true
  ```
  Restriction lints without an `allow-*-in-tests` flag
  (`clippy::todo`, `clippy::unimplemented`, `clippy::unreachable`)
  stay warned everywhere — tests have no legitimate use for them.

  Per-site `#[expect(...)]` / `#[allow(...)]` overrides in production
  are a high-friction escape hatch. Each requires a substantive
  `reason = "..."` naming the specific invariant being violated and
  why this site cannot meet it. Generic reasons (`"for now"`,
  `"TODO"`, `"tests use panicking helpers"`) are rejected at review.
  Audit: `rg '#\[(expect|allow)' -trust`.

### Type discipline (RS-4..RS-8)

- **RS-4** — *Per-module error enums via `thiserror` + `displaydoc`.*
  Messages go in doc comments — never `#[error("...")]` attributes.
- **RS-5** — *Nested directory module structure.* No central
  `types.rs` or `error.rs`. Types and errors live in the module that
  owns them. `lib.rs` has `pub mod` declarations only.
- **RS-6** — *Parse, Don't Validate.* Raw bytes and strings are parsed
  into typed, constrained representations at the boundary; downstream
  code never touches raw input.
- **RS-7** — *Newtypes for identifiers.* No bare `String` for IDs,
  codes, or domain numbers. Each newtype validates at construction;
  invalid input becomes a typed parse error, never a wrapped invalid
  value.
- **RS-8** — *Never `derive(From)` / `derive(Into)` on newtypes.* It
  bypasses validation. Use `#[from]` only on error enum variants.

### Banned in production (RS-9..RS-14)

- **RS-9** — *No panicking macros in production code.* `unwrap()`,
  `expect()`, `panic!()`, `todo!()`, `unimplemented!()`, and
  **`unreachable!()`** are banned in non-test code. Return an error
  variant instead — `Error::Unimplemented { ... }` for stubs,
  `Error::Bug { ... }` for "can't happen" branches. "Can't reach
  here" is a proof obligation for the type system, not a runtime
  macro. Enforced mechanically via the RS-3 clippy restriction
  lints.
- **RS-10** — *`#[allow(dead_code)]` is banned.* Use
  `#[expect(dead_code, reason = "...")]` so the compiler tells you
  when the expectation is unfulfilled.
- **RS-11** — *No silent error swallows.* `Result::ok()`,
  `let _ = result`, and `.unwrap_or_default()` on `Result` discard
  error information and let downstream code mistake failure for an
  empty value. Errors must be either handled with user-visible
  feedback or propagated with `?` after mapping to a typed variant.
  Exceptions: provably infallible sinks (`write!` to `String`,
  `writeln!` to `Vec<u8>`) with an inline comment naming why the sink
  can't fail.

  *Trap to watch:* `mutex.lock().ok()?` turns "another thread
  panicked holding this mutex" into "no value" — map to a
  `LockPoisoned` variant and `?` instead.
- **RS-12** — *No placeholder / sentinel values inside newtypes.* A
  newtype's invariants must hold for every instance. "Must be
  overwritten before use" is a type-system failure, not a runtime
  contract — thread the real value through (or refactor the type)
  rather than manufacturing a poison-pill instance.
- **RS-13** — *`Default` only when the zero-value is safe to use.*
  If a value must be overwritten before use, it's not a default —
  name the constructor for what it is (`placeholder()`, `pending()`,
  `for_test()`) so call sites read honestly. `Type::default()` must
  mean "the caller accepts the default", not "I owe a real value
  later".
- **RS-14** — *No test-fixture shape in production trait impls.*
  Test-only constructors live behind `#[cfg(test)]` (or in
  `tests/common/`); production types do not carry shape that exists
  only to satisfy test fixtures.

### Logging (RS-15)

- **RS-15** — *Use `tracing` with structured fields.* Log level
  signals whether processing continued:
  - `error!` — operation failed and did not continue
  - `warn!` — something went wrong but processing continued
  - `info!` — normal operational events (startup, request received)
  - `debug!` / `trace!` — development diagnostics
  - `error!(target: "loom::bug", ...)` — invariant violation that
    should never happen

  Every log carries structured fields identifying what was being
  processed (`%bead_id`, `%spec_label`, etc.). Variable *names* may
  be logged; environment-variable values, tokens, and API keys are
  never logged — wrap any secret-bearing value in a `Redacted(&str)`
  whose `Debug` impl prints `[REDACTED]`.

### Naming (RS-16)

- **RS-16** — *Avoid stutter naming.* Don't repeat a module's name in
  its type or function names: prefer `event::Envelope` over
  `event::EventEnvelope`, `state::Db` over `state::StateDb`. Clippy's
  `module_name_repetitions` (pedantic, via RS-3) catches accidental
  cases. Per-site
  `#[expect(clippy::module_name_repetitions, reason = "...")]` is
  allowed when the longer name reads better at the call site (e.g.
  re-exported at crate root where the module prefix isn't visible),
  but the `reason` must name that call-site context.

## Comments (COM-)

Cross-language: applies to shell, nix, rust, and any other source we keep here.

- **COM-1** — *Inline comments inside function bodies are a code
  smell.* Restructure first: a clearer name, a smaller function, a
  typed boundary, an enum variant — these survive renames and
  refactors. Comments rot. When an inline comment is genuinely the
  right answer (non-obvious invariant, workaround for a specific
  bug, citation of a spec/RFC/issue), keep it to one line. Multi-line
  prose blocks explaining what code does are banned.
- **COM-2** — *Doc comments describe contract, not implementation.*
  Module-, type-, function-, and public-API doc comments are fine
  when they describe what callers can assume and what invariants
  hold. They are *not* the place to explain what the code does —
  that's the code's job. Keep them concise; no multi-paragraph
  prose. Doc-comment messages on `thiserror` error variants follow
  RS-4 (`Display` text in doc comments, not `#[error("...")]`).
