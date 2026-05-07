# Loom Harness

Rust workspace, build system, templates, and dual-path transition for the Loom
agent driver.

## Problem Statement

Ralph's workflow (plan, todo, run, check, msg, spec) is implemented as bash
scripts with string-typed data, implicit contracts, and runtime template
rendering. This creates limited type safety — state management, template
variables, bead lifecycle, and error handling are fragile. Loom replaces these
with a Rust binary that owns the full workflow, with compile-time template
validation and typed domain objects.

This spec covers the platform: crate structure, Rust conventions, Nix
integration, Askama templates, SQLite state store, beads CLI wrapper, and the
dual-path transition strategy. The agent abstraction layer (pi-mono and Claude
Code backends, container communication, backend selection) lives in
[loom-agent.md](loom-agent.md). Workflow semantics (what each command does) are
defined in [ralph-loop.md](ralph-loop.md) and
[ralph-review.md](ralph-review.md) — Loom reimplements those semantics in Rust.

## Requirements

### Functional

1. **Command set** — Ralph workflow commands ported to Rust, plus a small set
   of auxiliary state/log management commands:

   **Workflow commands (driver phases):**
   - `loom plan` — spec interview (interactive agent session); flags
     `-n <label>` for a new spec and `-u <label>` for updating an existing
     one. No hidden-spec flag: scratch / private specs are kept out of
     git via `.git/info/exclude` rather than a separate spec home.
   - `loom todo` — spec-to-beads decomposition
   - `loom run` — execute beads in loop (continuous or `--once`)
   - `loom check` — review gate + push control
   - `loom msg` — clarify resolution
   - `loom spec` — query spec annotations; supports `--deps` to print
     nixpkgs required by the spec's `[verify]` / `[judge]` test files
     (port of `ralph sync --deps`)

   **Auxiliary commands (state / log management):**
   - `loom init` — create `.wrapix/loom/` config + state DB; `--rebuild`
     repopulates the state DB from `specs/*.md` and active beads
   - `loom status` — print active spec, current molecule, iteration count
     (trivial state DB query)
   - `loom use <label>` — set `current_spec` in the state DB; `loom status`
     reads it back
   - `loom logs` — tail the most recent bead JSONL log under
     `.wrapix/loom/logs/`; `--bead <id>` selects a specific bead

   **Ralph commands deliberately NOT ported:**
   - `ralph sync` / `ralph tune` — these manage per-project copies of bash
     mustache templates so users can customize them. Loom's templates are
     Askama, compiled into the binary; there is no per-project template
     copy to sync, back up, diff, or tune. Template updates ship via a new
     loom release.
   - `ralph watch` — polling observation daemon (tmux/browser monitoring
     that creates beads for detected issues). Independent feature, not
     part of the workflow phase set; deferred to a follow-up spec.
2. **Compiled templates** — Askama templates compiled into the binary. Ralph's
   markdown templates ported to Askama format with typed context structs.
   Template correctness verified at compile time.
3. **SQLite state store** — workflow state persisted in a SQLite database
   (`.wrapix/loom/state.db`). Tracks active specs, molecules, iteration
   counts, companions. Reconstructable from spec files on disk and active
   beads via `loom init --rebuild`.
4. **Beads integration** — interacts with beads via the `bd` CLI (subprocess
   calls). Bead operations: create, show, close, update, list, dep add, mol
   bond, mol progress. CLI output parsed into typed Rust structs.
5. **Profile selection** — reads `profile:X` labels from beads and resolves
   each label to a profile image via the
   [Profile-Image Manifest](#profile-image-manifest). Unknown labels fail
   at dispatch (no silent default). `--profile` overrides bead labels.
6. **Worktree parallelism** — `loom run --parallel N` (alias `-p N`) dispatches
   up to N ready beads concurrently, each in its own git worktree on a
   per-bead branch. After workers finish, branches are merged back to the
   driver branch sequentially. Default parallelism is 1 (sequential, current
   ralph behavior). Same model as `lib/ralph/cmd/run.sh` (`run_parallel_batch`).
7. **Retry with context** — on in-session worker failure, retries with the
   prior error output injected as the `previous_failure` template variable.
   Configurable max retries per bead (default 2). After in-session retries
   exhaust, the phase ends; the verdict is delegated to the
   [Verdict Gate](#verdict-gate).
8. **Verdict gate per phase** — `loom check` evaluates each phase's result
   through the verdict gate (see [Verdict Gate](#verdict-gate)) before the
   bead's state can advance. Mechanical signals (marker, closure, diff) and
   a single agent-judged review step feed the decision. Driver-detected
   gate failures enter a bounded recovery loop; agent self-reports
   (`LOOM_BLOCKED` / `LOOM_CLARIFY`) escalate directly to the human via
   `loom msg`.
9. **Push gate** — `loom check` pushes only when every bead in the molecule
   has reached `[done]` (no `loom:blocked` or `loom:clarify` outstanding).
   Auto-iterates on fix-up beads up to max iterations before refusing.
10. **Beads via shared Dolt socket** — every container has the host's
    `wrapix-beads` Dolt server bind-mounted at
    `/workspace/.wrapix/dolt.sock`; in-container `bd` writes go straight to
    the authoritative state. No per-bead `bd dolt push/pull` handoff. Loom
    on the host reads the same state through the same socket. The legacy
    `.beads/issues.jsonl` path is not used — beads no longer supports it.
11. **Spec resolution** — `--spec <name>` flag or fallback to the
    `current_spec` key in the state database.

### Non-Functional

1. **Rust edition 2024** with `resolver = "3"` at workspace root.
2. **Workspace-pinned dependencies** — every third-party crate pinned once
   under `[workspace.dependencies]`. Member crates use
   `foo = { workspace = true }`.
3. **Workspace-declared lints** — `[workspace.lints.rust]` and
   `[workspace.lints.clippy]` own the lint block. Every member declares
   `[lints] workspace = true`.
4. **Per-module error enums** using `thiserror` for `Error` and `displaydoc`
   for `Display`. Messages in doc comments, not `#[error("...")]`.
5. **Nested directory module structure** — no central `types.rs` or `error.rs`.
   Types and errors live in the module that owns them. `lib.rs` has `pub mod`
   only.
6. **Parse, Don't Validate** — raw strings parsed into typed representations at
   boundaries. Downstream code never touches raw input.
7. **Newtypes for identifiers** — `BeadId`, `SpecLabel`, `MoleculeId`,
   `ProfileName` for domain identifiers; `SessionId`, `ToolCallId`,
   `RequestId` for protocol identifiers. No bare `String` for typed IDs.
   `AgentKind` is an enum (`Pi`, `Claude`), not a newtype.
8. **No `derive(From)` or `derive(Into)` on newtypes** — bypasses validation.
   `#[from]` only on error enum variants.
9. **No panics in production** — `unwrap()`, `todo!()`, `unimplemented!()`,
   `panic!()` banned. Return error variants. `#[expect(dead_code)]` not
   `#[allow(dead_code)]`.
10. **Structured logging via `tracing`** — log level signals continuation:
    `error!` = failed, `warn!` = continued, `info!` = operational,
    `debug!`/`trace!` = diagnostics. Every log carries structured fields.
    Environment variable values and API keys are never logged — use a
    `Redacted(&str)` wrapper that implements `fmt::Debug` as `"[REDACTED]"`
    for any value that could contain secrets. Variable *names* may be logged.
11. **Nix integration** — built via `wrapix.profiles.rust.buildPackage`
    (crane-backed; see [profiles.md — Rust package builder](profiles.md#rust-profile)).
    `packages.loom` consumes `.bin` so devshell rebuilds skip the clippy/nextest
    passes; those land as separate `loom-clippy` / `loom-nextest` entries in
    `nix flake check`. Binary included in the devShell alongside (not replacing)
    Ralph's bash scripts during transition.

## Architecture

### Process Architecture

Loom is a host-side orchestrator. Every workflow phase that drives an agent
spawns its own container per bead — no shared long-lived container, no
in-container loom loop. The two motivations:

1. **Per-bead profile selection.** Beads carry `profile:rust` /
   `profile:python` / `profile:base` labels. Each bead must run in a container
   built from the matching profile image. A long-lived parent container can't
   change profile mid-run; per-bead spawn is the only clean way.
2. **Trust boundary.** Loom (orchestrator, on host) is trusted; the agent
   (claude or pi, in container) is the sandboxed execution layer.

**Container spawn is delegated to `wrapix spawn`** — a thin wrapix
subcommand that owns container construction (mounts, env passthrough, krun
runtime selection on aarch64 microVM, network filtering, deploy key, beads
dolt socket). Loom never invokes `podman run` directly. Nix remains the source
of truth for container layout; loom owns only the typed `SpawnConfig` it
hands to the wrapper.

```
loom (host)
    │
    ├─ build SpawnConfig (image_ref, image_source, env allowlist, mounts, scratch_dir)
    ├─ serialize to /tmp/loom-<id>.json
    │
    ├─ spawn: wrapix spawn --spawn-config /tmp/loom-<id>.json --stdio
    │   │
    │   └─ exec podman run [no -t, stdio piped] <image> <entrypoint>
    │       │
    │       └─ entrypoint.sh → agent (claude --print … / pi --mode rpc)
    │           ↑              ↓
    │           └── JSONL over stdin/stdout ─→ loom (parses events)
    │
    └─ on bead completion: container exits, next bead → next spawn
```

`wrapix spawn --stdio` is the non-TTY counterpart of today's interactive
`wrapix run` (which uses `podman run -it`). Both modes share container
construction; they differ only in stdio attachment. The
`--spawn-config <file>` flag accepts a JSON file that mirrors loom's typed
`SpawnConfig` — avoiding a fat argv interface and giving loom a single
serialization boundary.

**`loom plan` is the exception.** It is an interactive spec interview
(human-in-the-loop terminal session), so it shells out to interactive
`wrapix run` rather than driving an JSONL session. Loom prepares the
template-rendered prompt, sets environment, exec's `wrapix run`, and lets
claude attach to the user's terminal. No subprocess capture, no JSONL.

**Trade-off accepted:** parallelism is straightforward (N concurrent
`wrapix spawn` invocations) but per-bead container spawn cost (~1-2s on
podman) replaces ralph's per-iteration claude spawn inside one long-lived
container. For typical bead sizes (minutes of agent work), this is dominated
by agent runtime.

### Worktree Parallelism

`loom run --parallel N` is v1-parity with ralph's `run_parallel_batch` (see
`lib/ralph/cmd/run.sh`):

1. Pull up to N ready beads (`bd ready --limit=N`).
2. For each bead, create a git worktree at
   `.wrapix/worktree/<label>/<bead-id>/` on a fresh branch
   `loom/<label>/<bead-id>` based on HEAD.
3. Spawn one `wrapix spawn --spawn-config <file> --stdio` per worktree
   concurrently. Each container's workdir bind mount points at the worktree
   path, not the main checkout.
4. `tokio::join!` (or `JoinSet`) on the futures; collect per-bead results.
5. Merge finished bead branches back to the driver branch **sequentially**
   (single-threaded merge avoids index lock contention). On merge conflict,
   the bead is marked failed and the worktree is preserved for inspection.
6. On agent failure, the worktree branch is cleaned up (deleted) and the
   bead is retried per the retry policy.

`--parallel 1` is the default and behaves exactly as today's sequential run
(no worktree, work happens on the driver branch). `--parallel N` for `N > 1`
always uses worktrees, even for a single ready bead in that batch.

**Git operations: hybrid `gix` + `git` CLI.** Worktree, branch, status,
and merge operations go through a typed `GitClient` in `loom-core`. The
implementation is hybrid, encapsulated inside the module; callers see only
typed Rust methods.

| Operation | Backend | Reason |
|-----------|---------|--------|
| `status` (working tree vs HEAD) | [`gix`](https://docs.rs/gix) `Repository::status()` | mature (`crate-status.md`: checked) |
| `diff` (HEAD vs HEAD~) | `gix::diff` (`blob-diff` feature) | mature |
| List refs / branches | `gix::Repository::references()` | mature |
| Read commit graph / HEAD | `gix` | mature |
| List worktrees | `gix::Repository::worktrees()` | mature (open/iter only) |
| **Create worktree + branch** | `git worktree add -b` (CLI) | `gix` worktree create/remove unchecked in `crate-status.md` |
| **Remove / prune worktree** | `git worktree remove` / `prune` (CLI) | same |
| **Merge bead branch back** | `git merge` (CLI) | `gix-merge` writes a merged tree but cannot persist `MERGE_HEAD`/`MERGE_MSG` (unchecked); avoids reimplementing the index dance |

`gix` 0.83+ is pinned with features `["status", "blob-diff", "revision",
"parallel", "sha1"]` (the `sha1` feature is required for gix-hash to compile;
without it the `Kind` enum has no variants). For tokio integration:
`gix::Repository` is `!Sync`; loom holds a
`ThreadSafeRepository` and clones a thread-local handle inside
`spawn_blocking` per call. CLI shell-outs use `tokio::process::Command`
with arguments passed via `.arg()` — never shell interpolation — and a
60-second timeout matching `BdClient`.

The hybrid line is reviewed each loom release; gix operations migrate
inward as the corresponding `crate-status.md` items become checked.

### Profile-Image Manifest

The *profile-image manifest* is a JSON file produced by Nix at flake-build
time that maps each profile name to the podman ref and Nix store path
needed to spawn its image. Loom reads it at startup and, for each bead,
looks up the profile label to populate `SpawnConfig.image_ref` (the podman
ref) and `SpawnConfig.image_source` (the store path handed to
`podman load`).

The file is a JSON object keyed by profile name, with two string fields
per entry:

```json
{
  "base":   { "ref": "localhost/wrapix-base:abc123",   "source": "/nix/store/...-image-base" },
  "rust":   { "ref": "localhost/wrapix-rust:def456",   "source": "/nix/store/...-image-rust" },
  "python": { "ref": "localhost/wrapix-python:ghi789", "source": "/nix/store/...-image-python" }
}
```

Built by `wrapix.lib.${system}.mkProfileImages` (defined in
[profiles.md](profiles.md)); the bundled flake output is
`packages.profile-images`. External flakes that add custom profiles call
`mkProfileImages` themselves to produce a manifest covering their full
profile set.

Loom reads the manifest path from the `LOOM_PROFILES_MANIFEST` environment
variable. The bundled devshell sets it to `${self'.packages.profile-images}`;
consumers integrating loom into their own flake set it the same way. If the
variable is unset or the file is missing, loom errors at startup before any
bead spawn — there is no implicit search path or fallback default. The
manifest is parsed once at startup and held as a
`BTreeMap<ProfileName, ImageEntry>` in `loom-core`.

Per-bead dispatch is:

1. Parse the bead's labels; pick the highest-precedence `profile:X` (or the
   value of `--profile` if set on the CLI).
2. Look up `X` in the parsed manifest. Missing key → typed
   `ProfileError::UnknownProfile { name, manifest_path }`.
3. Build `SpawnConfig` with `image_ref = entry.ref` and `image_source =
   entry.source`. Hand it to `wrapix spawn`.

Agent (claude vs pi) is selected at container start via the `WRAPIX_AGENT`
env-allowlist entry the entrypoint switches on — see
[loom-agent.md — Agent Runtime Layer](loom-agent.md#agent-runtime-layer).
The manifest stays one-dimensional; each per-profile image carries both
runtimes, and `mkSandbox` no longer takes an `agent` parameter at Nix-eval
time.

`loom plan` is interactive, so it shells out to `wrapix run` (TTY-attached)
rather than `wrapix spawn`. To keep one resolution path, plan looks up its
profile (per [Configuration](#configuration); default `base`) in the
manifest and exports `WRAPIX_DEFAULT_IMAGE_REF=<entry.ref>` plus
`WRAPIX_DEFAULT_IMAGE_SOURCE=<entry.source>` into the child environment
before exec'ing `wrapix run`. The launcher reads those env vars when no
`--spawn-config` is supplied — see
[sandbox.md — Launcher Subcommands](sandbox.md#launcher-subcommands).

### Concurrency & Locking

Multiple `loom` invocations on the same workspace are explicitly allowed.
The lock model is **per-spec advisory locks** plus a single workspace
exclusive lock used only during destructive state rebuild.

**Lock files** live **outside the workspace**, under
`$XDG_STATE_HOME/loom/locks/<workspace-basename>/` (default
`~/.local/state/loom/locks/<workspace-basename>/`):

- `<label>.lock` — one per spec
- `workspace.lock` — held by `loom init` and `loom init --rebuild`
  (`workspace` is reserved as a spec label to avoid collision)

`<workspace-basename>` is the final path component of the canonicalized
workspace root (e.g. `/workspace` → `workspace`, `~/work/myrepo` →
`myrepo`). Two workspaces with the same basename share a lock namespace;
this is accepted as a known limitation in exchange for human-readable,
greppable lock paths.

Lock files **must not** live inside the workspace, because the bead
container has the workspace bind-mounted read-write and could `rm` the
lock file out from under the host driver, silently breaking mutual
exclusion. Putting locks under `$XDG_STATE_HOME` keeps them on the host
filesystem only.

All locks are POSIX advisory locks acquired via `flock(2)` through the
`fd-lock` crate. The kernel releases them on process exit or crash, so
there are no stale locks to clean up.

**Lock matrix:**

| Class | Commands | Lock acquired |
|-------|----------|---------------|
| Read-only | `status`, `logs`, `spec` | none |
| Spec-scoped mutating | `plan`, `todo`, `run`, `check`, `msg`, `use` | exclusive on `<label>.lock` |
| Workspace-exclusive | `init`, `init --rebuild` | exclusive on `workspace.lock` |

A spec-scoped command on label `X` waits up to 5 seconds for `<X>.lock`,
then errors with `another loom command is operating on <X>` (no busy-loop,
no silent stalls). `init` and `init --rebuild` error immediately if any
spec lock is held.

**Why git is the second-order serialization point.** Two `loom run`
invocations on *different* specs share the driver git branch. They will
collide briefly at merge-back and push:

- Concurrent `git merge` is serialized by git's own `index.lock`; the
  losing process surfaces a clear error and retries.
- Concurrent `git push` from `loom check` produces non-fast-forward on
  the second push; `check`'s push gate re-fetches and retries.

These are accepted, recoverable failure modes — not silent corruption —
which is why a workspace-wide lock is *not* required for `run`/`check`.

### Nested-Loom Guard

The driver sets `LOOM_INSIDE=1` in every bead container's environment
(passed through the `SpawnConfig.env` allowlist — see
[loom-agent.md](loom-agent.md)). On startup, `loom` checks this env var
and, if set, refuses to run **container-spawning or workspace-mutating**
subcommands with a clear error:

```
error: loom cannot run inside a loom-managed container
  this command spawns containers or mutates workspace state, which
  would create a nested driver. read-only commands (status, logs,
  spec) are still available.
```

**Refused inside container:** `run`, `init`, `plan`, `check`, `todo`,
`msg`, `use`.

**Allowed inside container:** `status`, `logs`, `spec` (read-only;
useful for an agent inspecting bead history during its own task).

The guard is mechanical, not advisory: a single env-var check at CLI
entry, before any subcommand dispatch.

### Run UX & Logging

`loom run` is the only long-running command users watch live. Its terminal
output is shaped for a human reading along, not for machine parsing.

**Default terminal output** (one bead at a time, sequential or parallel):

```
▸ wx-abc123  Implement parser    [profile:rust]
  Read    src/lib.rs
  Read    src/parser/mod.rs
  Edit    src/parser/mod.rs +42 -3
  Bash    cargo test --lib
  ✓ done  (3 tool calls, 47s)
```

Rules:

- One header line per bead: id, title, profile.
- One line per tool call: tool name + a short summary (path, range, command
  prefix). Long values are truncated to one line.
- Assistant text deltas are **not** streamed live by default. The final
  assistant message is summarized in the closing `✓ done` / `✗ failed` line.
- `--verbose` (or `-v`) escalates to live streaming: assistant text deltas
  print as they arrive, plus tool call args inline.
- Color is used for status only (green ✓, red ✗, yellow for retry); tool
  output stays plain so logs are grep-friendly.
- With `--parallel N`, header/finish lines are printed atomically; tool
  call lines are tagged with the bead id prefix so interleaved output
  remains attributable.

**Log persistence.** Loom always writes the **full raw JSONL** event stream
for every bead to disk, regardless of terminal verbosity. One file per bead
spawn:

```
.wrapix/loom/logs/<spec-label>/<bead-id>-<utc-timestamp>.jsonl
```

Per-bead (not per-session) so parallel batches never interleave inside a
single file. The path is logged at `info!` when the spawn starts so users
can `tail -f` it.

**Retention.** Logs are swept on `loom run` startup: any file under
`.wrapix/loom/logs/` whose mtime is older than `[logs] retention_days`
(default 14) is deleted. `retention_days = 0` disables sweeping (keep
forever). The sweep is best-effort and logged at `debug!` — failures to
delete (permission, in-use file) do not abort the run. Sweeping runs once
per `loom run` invocation, before any bead spawns; the cost is a single
directory walk.

The terminal renderer consumes the same `AgentEvent` stream that's written
to disk — there's a single tee-style sink, not two parallel pipelines.

### Verdict Gate

After every agent phase ends, `loom check` evaluates the result through a
deterministic gate before the bead's state can advance. The gate combines
mechanical signals (cheap, no LLM call) with a single agent-judged review
step. Driver-detected failures enter a bounded recovery loop; agent
self-reports go straight to human resolution via `loom msg`.

**Decision table.** The gate inspects four signals — the agent's exit marker,
whether the bead was bd-closed, whether the worktree diff is empty, and the
review verdict — and produces one of four outcomes (`done`, `blocked`,
`clarify`, or `recovery` with a cause):

| Marker | bd-closed | Diff | Review | Outcome |
|--------|-----------|------|--------|---------|
| `LOOM_BLOCKED` | — | — | — | `blocked` |
| `LOOM_CLARIFY` | — | — | — | `clarify` |
| (none) | — | — | — | recovery (`swallowed-marker`) |
| `LOOM_COMPLETE` | no | — | — | recovery (`incomplete-signaling`) |
| `LOOM_COMPLETE` | yes | empty | — | recovery (`zero-progress`) |
| `LOOM_COMPLETE` | yes | non-empty | verify-fail (review may also flag) | recovery (`verify-fail`; review notes appended if any) |
| `LOOM_COMPLETE` | yes | non-empty | verify-pass + review-flag | recovery (`review-flag`) |
| `LOOM_COMPLETE` | yes | non-empty | verify-pass + review-pass | `done` |
| `LOOM_NOOP` | yes | * | verify-fail (review may also flag) | recovery (`verify-fail`; review notes appended if any) |
| `LOOM_NOOP` | yes | * | verify-pass + review-flag | recovery (`review-flag`) |
| `LOOM_NOOP` | yes | * | verify-pass + review-pass | `done` |

`recovery` resolves to `retry` if the bead's iteration counter is below
`[loop] max_iterations` (default 3), otherwise `blocked` with the cause
preserved in `bd update --notes`. The iteration counter is bead-level state
and survives `retry → [running]` round-trips.

**Mechanical vs review.** Marker parsing, bd-closed lookup, and diff
inspection are deterministic. The gate then runs **every** `[verify]`
script attached to the bead's success criteria (see
[live-specs.md](live-specs.md)) — none short-circuit each other. Per
script, the gate captures pass/fail + stderr.

**Review always runs**, regardless of `[verify]` results. If verify
failed, review still runs so the agent gets verify failures *and*
live-path / scope / `[judge]` feedback in one `previous_failure` round
trip — otherwise the agent might "fix" a failing test by mocking
harder and reach `done` on the next iteration before review catches it.

When verify fails, the recovery cause is `verify-fail` (mechanical
trumps semantic), and review's flag reasoning, if any, is appended to
the `previous_failure` detail under a `Review notes:` heading.

Its inputs are:

- the diff
- the bead's intent (title, description, success criteria bullets)
- the spec's `## Affected Files` and surrounding spec context
- the **source** of every `[verify]` script the gate just ran
- the **rubric text** of every `[judge]` annotation on the bead's criteria

Its primary concern is **live-path coverage**, governed by two rules:

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

Secondary concerns review also judges:

- **Scope appropriateness** — does the diff match the bead's intent and stay
  reasonably close to `## Affected Files`?
- **`[judge]` rubrics** — does the work satisfy each LLM-judgement criterion?

Output is a structured pass/flag verdict. A flag from any concern
produces `recovery` with cause `review-flag`. The flag detail names which
specific concern triggered it (e.g. `live-path: test mocks the agent
backend instead of spawning it`).

**Self-reports skip recovery.** `LOOM_BLOCKED` and `LOOM_CLARIFY` are agent
self-reports — re-running the same prompt won't recover, so the gate exits
straight to `[blocked]` / `[clarify]` for human resolution.

**Driver-detected causes flow through recovery.** Swallowed marker,
incomplete signaling, zero-progress, verify-fail, and review-flag all
enter the recovery loop. Each recovery iteration spawns a fix-up bead
or retries the bead with prior failure context.

**Recovery context (`previous_failure`).** On `retry → [running]`, the next
session's prompt is rendered with `previous_failure` populated as a
structured cause + per-cause detail (truncated to 4000 chars):

| Cause | Detail content |
|-------|----------------|
| `swallowed-marker` | "Last phase ended without a `LOOM_*` exit marker." (no further detail) |
| `incomplete-signaling` | "Marker `LOOM_COMPLETE` emitted but bead `<id>` was not bd-closed." |
| `zero-progress` | "Marker `LOOM_COMPLETE` emitted with empty diff. Use `LOOM_NOOP` if no work was needed." |
| `verify-fail` | One block per failing `[verify]` script: path, exit code, last ~40 lines of stderr. All failing scripts are included; the 4000-char budget is split across them with later failures truncated first. If `review` also flagged, its reasoning is appended under `Review notes:` (separate budget, ~1000 chars). |
| `review-flag` | The review LLM's verbatim flag reasoning (typically 1–3 sentences), including which concern (live-path / mock / scope / judge) triggered the flag. |

Transcript excerpts are deliberately not included — the agent can re-read
its own session log if it needs prior tool-call context.

**Labels.**

- `loom:blocked` is applied by either: (a) the `LOOM_BLOCKED` agent marker, or
  (b) driver-detected gate failure with recovery exhausted. Both meanings are
  uniform from the human's perspective — the bead is blocked and `loom msg`
  is the resolution channel.
- `loom:clarify` is applied only by the `LOOM_CLARIFY` agent marker — the
  agent has a specific question with structured options for the human.
- The cause of a driver-applied `loom:blocked` (`swallowed-marker`,
  `incomplete-signaling`, `zero-progress`, `verify-fail`, `review-flag`,
  `retry-exhausted`) is preserved in the bead's notes. Per-cause sub-labels
  can be stacked on top later if filtering becomes important; the gate's
  terminal label stays `loom:blocked`.

**Marker definitions.** `LOOM_NOOP` is a new exit signal — the agent emits it
when the work is already done and the phase intentionally produces an empty
diff. Without `LOOM_NOOP`, an empty diff is treated as zero-progress.
`LOOM_COMPLETE` / `LOOM_BLOCKED` / `LOOM_CLARIFY` retain their existing
meanings (see [ralph-review.md](ralph-review.md)).

**Infra failures bypass the gate.** Pre-flight failures (image load, container
start) exit immediately as `blocked` with cause `infra-preflight` — there is
no agent output to evaluate. Mid-session failures (agent process exit
non-zero, container OOM, IO errors) get one free retry per `loom run`,
tracked in driver memory; a second mid-session failure exits as `blocked`
with cause `infra-repeated`. This counter is separate from
`[loop] max_iterations` and does not persist across `loom run` invocations.

### Crate Layout

```
loom/
  Cargo.toml                    # workspace root
  crates/
    loom/                       # CLI binary — clap arg parsing, entry point,
      src/                      #   match on AgentKind, wires concrete
        main.rs                 #   backends + workflow + templates
    loom-core/                  # shared types: BeadId, SpecLabel, MoleculeId,
      src/                      #   AgentBackend trait, AgentEvent enum,
        lib.rs                  #   AgentSession, LineParse, JsonlReader,
                                #   state db, config, bd CLI wrapper
    loom-agent/                 # AgentBackend trait implementations
      src/                      #   (see loom-agent.md)
        lib.rs
    loom-workflow/              # workflow engine: plan, todo, run, check,
      src/                      #   msg, spec — owns the orchestration loop,
        lib.rs                  #   bead lifecycle, retry logic, push gate
    loom-templates/             # askama templates compiled from ralph's
      src/                      #   markdown; typed context structs per
        lib.rs                  #   template
      templates/                # askama template files (.md)
        plan_new.md
        plan_update.md
        todo_new.md
        todo_update.md
        run.md
        check.md
        msg.md
        partial/
          context_pinning.md
          exit_signals.md
          spec_header.md
          companions_context.md
          scratchpad.md
          ...
```

### Dependency Graph

```
loom (CLI binary)
  ├── loom-core
  ├── loom-agent
  ├── loom-workflow
  └── loom-templates

loom-agent
  └── loom-core

loom-workflow
  ├── loom-core (AgentBackend trait, AgentEvent, newtypes)
  └── loom-templates

loom-templates
  └── loom-core (for typed context structs)
```

### Workspace Dependencies

All third-party crates pinned once under `[workspace.dependencies]`. Member
crates use `foo = { workspace = true }`.

```toml
[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = { version = "1", features = ["raw_value"] }
thiserror = "2"
displaydoc = "0.2"
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
rusqlite = { version = "0.32", features = ["bundled"] }
toml = "0.8"
askama = "0.16"
clap = { version = "4", features = ["derive"] }
gix = { version = "0.83", default-features = false, features = ["status", "blob-diff", "revision", "parallel", "sha1"] }
fd-lock = "4"
```

Fourteen dependencies. No JSONL-specific crate — `serde_json` + `BufReader` line
splitting is sufficient. No `async-trait` — `async fn` in traits is stable and works
natively with static dispatch. `gix` covers read-only git operations (status,
diff, refs, commit graph, worktree iteration); worktree mutation and merge
shell out to the system `git` CLI — see *Worktree Parallelism* for the split.

### Parse, Don't Validate

Raw data enters typed domain representations at the boundary and stays typed
everywhere downstream. No internal function re-checks or re-parses.

**Boundary layers (outside → inside):**

1. **JSONL framing** — `BufReader::read_line` splits the byte stream into
   lines. Each line is one JSON object.
2. **Protocol parsing** — `serde_json::from_str` deserializes each line into a
   backend-specific message type (`PiMessage` or `ClaudeMessage`).
3. **Event normalization** — backend-specific messages map to `AgentEvent`.
   After this point, no code knows which backend is running.
4. **Domain newtypes** — identifiers (`BeadId`, `SpecLabel`, etc.) are parsed
   from strings at construction. Downstream code receives `BeadId`, never
   `String`.
5. **State queries** — SQLite rows map to typed Rust structs via `rusqlite`.
   No intermediate untyped step.
6. **CLI output parsing** — `bd --json` output deserializes into typed structs
   (`Bead`, `Molecule`).
7. **Profile-image manifest** — the JSON produced by `mkProfileImages`
   deserializes into `BTreeMap<ProfileName, ImageEntry { ref, source }>` once
   at loom startup. Downstream code receives `&ImageEntry`, never raw JSON.

**Newtype IDs:**

Each identifier in `loom-core::identifier` is hand-written (no shared macro)
so per-type parse rules can be enforced at construction. Every newtype wraps
a single `String`, exposes `as_str() -> &str`, implements `Display` as the
inner string, and derives the standard value traits (`Debug`, `Clone`,
`PartialEq`, `Eq`, `Hash`) plus `#[serde(transparent)]` so it serializes as
a plain string — no wrapper object.

`BeadId` additionally validates the canonical
`<prefix>-<base32>(.<digits>)?` shape at every construction path: `new`
returns `Result<Self, ParseBeadIdError>`, and `Deserialize` is hand-written
to reject malformed input rather than constructing an invalid wrapper.
Other newtypes (`SessionId`, `ToolCallId`, `RequestId`, `SpecLabel`,
`MoleculeId`, `ProfileName`) keep a permissive `new(impl Into<String>)`.

`derive(From)` and `derive(Into)` are banned (NF-8) to prevent accidental
bypass of the newtype boundary.

### Askama Template System

Ralph templates use `{{VARIABLE}}` and `{{> partial}}` syntax. Askama uses
`{{ variable }}` and `{% include "partial.md" %}`. Migration is mechanical:

- `{{LABEL}}` becomes `{{ label }}`
- `{{> context-pinning}}` becomes `{% include "partial/context_pinning.md" %}`
- Each template gets a typed struct with all variables as fields
- Partials become standalone Askama templates included via `{% include %}`
- Agent-generated content (`previous_failure`, `title`, `description`,
  `existing_tasks`) is delimited with `<agent-output>` / `</agent-output>`
  markers in templates to help the receiving agent distinguish injected
  content from system instructions. This is a best-effort mitigation —
  prompt injection via retry context is an inherent risk of the
  retry-with-context pattern and is accepted because the agent runs inside
  a sandboxed container regardless.

Template variables (sourced from
[ralph-harness.md](ralph-harness.md#template-variables)):

| Variable | Type | Used By |
|----------|------|---------|
| `pinned_context` | `String` | all |
| `label` | `SpecLabel` | all |
| `spec_diff` | `Option<String>` | todo-update |
| `existing_tasks` | `Option<String>` | todo-update |
| `companion_paths` | `Vec<String>` | plan-update, todo-*, run, check, msg |
| `clarify_beads` | `Vec<ClarifyBead>` | msg |
| `implementation_notes` | `Vec<String>` | todo-new, todo-update |
| `molecule_id` | `Option<MoleculeId>` | todo-update, run |
| `issue_id` | `Option<BeadId>` | run |
| `title` | `Option<String>` | run |
| `description` | `Option<String>` | run |
| `beads_summary` | `Option<String>` | check |
| `base_commit` | `Option<String>` | check |
| `previous_failure` | `Option<String>` | run (retry only, truncated to 4000 chars) |
| `exit_signals` | `String` | all (via partial) |
| `scratchpad_path` | `String` | all (via partial) |

### Beads CLI Wrapper

`loom-core` provides `BdClient`, a typed wrapper around the `bd` CLI:

- Invokes `bd` via `tokio::process::Command` with each argument passed via
  `.arg()`. No shell interpolation — values from agent output (bead titles,
  error messages, labels) must never be passed through `sh -c` or string
  interpolation into a shell command. This prevents injection of shell
  metacharacters from agent-controlled content.
- Uses `--json` flag where available
- Parses output into typed structs (`Bead`, `Molecule`, `MolProgress`).
  Bead labels deserialize into a `Label` newtype that pre-parses the
  `spec:`/`profile:`/`loom:clarify`/`loom:active` prefix families once at
  the boundary, so call sites read through typed accessors
  (`spec_label()`, `profile_name()`, `is_clarify()`, `is_active()`)
  rather than re-doing `strip_prefix` walks
- Maps CLI errors to typed error variants
- All subprocess calls have a 60-second timeout (configurable). Prevents
  unbounded hangs from a stuck `bd` process.
- Key operations: `show`, `create`, `close`, `update`, `list`, `dep_add`,
  `mol_bond`, `mol_progress`. No `dolt_push` / `dolt_pull` wrappers — loom
  relies on the bind-mounted Dolt socket so every `bd` call is already
  authoritative.

### SQLite State Store

Workflow state lives in `.wrapix/loom/state.db`. The schema is owned by
`loom-core` and migrated on open (embed migrations via `rusqlite`'s
`execute_batch`).

```sql
CREATE TABLE specs (
    label                TEXT PRIMARY KEY,
    implementation_notes TEXT  -- JSON array, nullable
);

CREATE TABLE molecules (
    id              TEXT PRIMARY KEY,
    spec_label      TEXT NOT NULL REFERENCES specs(label),
    base_commit     TEXT,
    iteration_count INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE companions (
    spec_label     TEXT NOT NULL REFERENCES specs(label),
    companion_path TEXT NOT NULL,
    PRIMARY KEY (spec_label, companion_path)
);

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- meta rows: current_spec, schema_version
```

Typed Rust API — no raw SQL outside `loom-core`. `loom-core` owns a single
`StateDb` handle that wraps the SQLite connection and exposes typed
operations: open the DB at a path, fetch a spec row by label, fetch the
active molecule for a spec label (zero or one), get/set the `current_spec`
meta key, increment a molecule's iteration counter, and run the rebuild
described below. Every operation returns `Result<_, StateError>`; row
shapes are `SpecRow` (label, implementation_notes) and `MoleculeRow` (id,
spec_label, base_commit, iteration_count) — i.e. the columns of the
`specs` and `molecules` tables, with `implementation_notes` already
deserialized from the JSON-array TEXT column into a typed list.

**Rebuild (`loom init --rebuild`):** Drops and recreates all tables, then
repopulates from three sources:

1. Glob `specs/*.md` → one `specs` row per file (label from filename).
   ~10-20 files.
2. `bd list --status=open --label=loom:active` → active molecules only
   (typically 0-3). For each, `bd mol progress <id>` → one `molecules` row.
3. Each spec markdown is parsed for a canonical `## Companions` section
   (see *Companion declaration in specs* below); each listed path becomes
   one `companions` row. Specs without the section contribute zero
   companions, not an error.

Iteration counters reset to 0 on rebuild. **Implementation notes are lost
on rebuild** — they're written by `loom plan` and have no external source
to reconstruct from. This is the only field with this property; recovering
notes after a rebuild requires re-running the relevant `loom plan` session.
Total cost: a glob + ~5 `bd` CLI calls + N markdown reads (already loaded
for source #1). Runs in under a second.

**Companion declaration in specs.** Specs declare their companion paths in
a single, parseable section so rebuild is lossless:

```markdown
## Companions

- `lib/sandbox/`
- `lib/ralph/template/`
```

Parser rules:

- Heading must be exactly `## Companions` (case-sensitive, level 2).
- Body is a flat bullet list of `- ` lines. Each path is a single
  backtick-delimited token; anything outside the backticks is ignored.
- Paths are normalized to repo-relative POSIX form (no leading `/`,
  trailing slash preserved if present).
- Missing section → zero companions for this spec (not an error).
- Malformed lines (no backticks, multiple paths) are skipped with a
  `warn!`, not an abort.

This is the only contract between spec authors and the state DB on
companions. The `loom plan` interview enforces the format when adding
companion paths to a spec.

**Implementation-notes lifecycle.** Implementation notes are *transient
implementer hints* — bug-or-gotcha context, file paths to touch, design
trade-offs left to the implementer's judgement. They guide `loom todo`'s
bead generation and are never canonical. The DB row is the single
source of truth; `loom todo` renders them into each new bead body.

Lifecycle:

| Event | Effect on `specs.implementation_notes` |
|-------|----------------------------------------|
| `loom plan -n <label>` | Creates the row; populates `implementation_notes` from the interview. |
| `loom plan -u <label>` | Row exists; the interview reads the existing `implementation_notes` array and writes back a **merged** array — keeping notes still relevant, dropping ones a new decision invalidates, adding fresh ones. The merge is the agent's judgement during the interview, not a blind append or replace. |
| `loom todo` | Reads notes, renders them into each new bead body, then sets `implementation_notes = NULL`. The row itself stays — molecules and companions still reference it. |
| `loom init --rebuild` | Wipes all notes (drop+recreate). Row is repopulated from `specs/*.md` glob with `implementation_notes = NULL`. |
| Spec file deleted from `specs/` | Row is orphaned. Cleanup deferred to next `--rebuild`. |

Rows are **never** deleted by routine commands — only by `--rebuild`.
Clearing notes (`UPDATE … SET implementation_notes = NULL`) is the only
mutation `loom todo` performs on the row; the row itself remains as a
foreign-key target for molecules and companions until rebuild.

**Todo cursor advancement.** `meta.todo_cursor:<label>` records the
last commit at which `loom todo` ran for a spec. Tier-2 detection uses
the cursor to scan only commits since the last run. Advancing it
prematurely silently demotes future invocations to no-ops on real work
the agent never saw, so the cursor is **only** advanced when the
session demonstrates productive completion:

- The agent emitted a `LOOM_COMPLETE` or `LOOM_NOOP` marker on its
  final turn (verdict-gate-recognised completion shapes).
- The session's `exit_code == 0`.

A zero exit alone is not enough — backend errors (529 overload,
network drop, watchdog timeout) and swallowed-marker turns also exit
zero, and treating those as success would skip the spec's commit
range on the next `loom todo` run. On any other terminal state
(`LOOM_BLOCKED`, `LOOM_CLARIFY`, missing marker, nonzero exit) the
cursor stays put so the next invocation reprocesses the same range.

**Container exposure:** The state DB is inside the workspace bind-mounted
into containers. A malicious agent could modify it directly. This is an
accepted risk — the DB is reconstructable via `loom init --rebuild`, the
blast radius is limited to iteration counters and `current_spec`, and the
durable sources of truth (spec files on disk, beads in Dolt) are
independently verifiable.

### Compaction Recovery

Compaction summarizes conversation history; anything that lived only in
conversation is lost. Recovery uses two pieces — the original phase prompt
(re-pinned verbatim) and a live scratchpad the agent writes to during the
session — joined by a hook script that re-injects both after compaction.

**Per-session scratch directory.** At session start the driver creates
`.wrapix/loom/scratch/<key>/`:

- `prompt.txt` — the initial prompt sent to the agent at session start.
- `scratch.md` — empty scratchpad. The agent appends decisions, open
  questions, and TODOs as the session progresses, per
  `partial/scratchpad.md`.
- `repin.sh` — small bash that emits the `SessionStart[compact]` JSON
  envelope: a short fixed preamble identifying this as a post-compaction
  re-pin, then `cat prompt.txt`, then `cat scratch.md`.

`<key>` is the session concurrency unit, matching the existing locks:
spec **label** for `loom plan -n` / `loom plan -u` / `loom todo`; **bead
ID** for `loom run` / `loom check` / `loom msg`. Two parallel run
workers on different beads of the same molecule get independent scratch
directories.

**Hook registration.** The driver writes a `claude-settings.json` fragment
registering `repin.sh` under `SessionStart[matcher: compact]`.
Compaction triggers the hook; the hook concatenates prompt + scratchpad
and claude re-pins both. Pi delivers the same payload via a `steer`
command on its native `compaction_start` event — see [loom-agent.md §
Compaction Handling](loom-agent.md#compaction-handling).

**Cleanup.** The driver removes the per-key scratch directory at session
end on every exit path. A new session for the same key starts
empty — no carry-over from a prior crashed session.

## Dual-Path Transition

During the transition period:

1. Both `ralph` (bash) and `loom` (Rust) are available in the devShell.
2. State is independent: Ralph uses `.wrapix/ralph/state/<label>.json`,
   Loom uses `.wrapix/loom/state.db`. No cross-system state sharing.
3. Both use the same beads database, `bd` CLI, and spec files.
4. Users can switch between them freely. Switching from Ralph to Loom on
   an in-flight spec requires `loom init --rebuild` to populate the DB.
5. `ralph` remains the default; `loom` is opt-in.
6. Once loom reaches feature parity and stability, `ralph` bash scripts are
   removed and `loom` is renamed to (or aliased as) `ralph`.

### Compatibility Constraints

- Bead labels, molecule IDs, and beads sync protocol are unchanged.
- Git commit conventions are unchanged.
- Container profile selection logic is unchanged.
- Entrypoint.sh changes are additive (agent selection conditional).

## Affected Files

### New

| File | Role |
|------|------|
| `loom/Cargo.toml` | Workspace root |
| `loom/crates/loom/` | CLI binary |
| `loom/crates/loom-core/` | Shared types, traits, bd wrapper, config, state db |
| `loom/crates/loom-agent/` | AgentBackend implementations (see [loom-agent.md](loom-agent.md)) |
| `loom/crates/loom-workflow/` | Workflow engine (plan, todo, run, check, msg, spec) |
| `loom/crates/loom-templates/` | Askama templates ported from Ralph |
| `.wrapix/loom/config.toml` | Loom config (TOML, independent of Ralph's Nix config) |
| `.wrapix/loom/state.db` | SQLite state database (created on first run) |

### Modified

| File | Change |
|------|--------|
| `flake.nix` | Add loom as a Rust package input |
| `modules/flake/packages.nix` | Build loom; expose `packages.wrapix`, `packages.image-<profile>`, `packages.sandbox-<profile>[-pi]`, `packages.profile-images` (see [profiles.md — Flake Outputs](profiles.md#flake-outputs)) |
| `modules/flake/devshell.nix` | Include loom; set `LOOM_PROFILES_MANIFEST=${self'.packages.profile-images}` |
| `lib/sandbox/default.nix` | Split `mkSandbox` into profile-agnostic launcher + per-profile images. Return shape `{ package, image, profile }` preserved. See [sandbox.md](sandbox.md) and [profiles.md](profiles.md) |
| `lib/sandbox/linux/default.nix` | Rename `wrapix run-bead` → `wrapix spawn`; add `image_source` load step (idempotent); read `WRAPIX_DEFAULT_IMAGE_REF`/`WRAPIX_DEFAULT_IMAGE_SOURCE` for `wrapix run` |
| `lib/sandbox/darwin/default.nix` | Darwin equivalent of the linux launcher changes (tarball image variant) |

### Unchanged (during dual-path phase)

| File | Reason |
|------|--------|
| `lib/ralph/cmd/*.sh` | Bash scripts remain functional |
| `lib/ralph/template/` | Templates remain for bash path |
| `lib/city/` | Gas City unchanged |

## Success Criteria

### Crate structure

- [ ] Workspace builds with `cargo build` from `loom/` root
  [verify](tests/loom-test.sh::test_workspace_builds)
- [ ] All five crates present: loom, loom-core, loom-agent, loom-workflow, loom-templates
  [verify](tests/loom-test.sh::test_crate_structure)
- [ ] Workspace uses edition 2024 and resolver "3"
  [verify](tests/loom-test.sh::test_workspace_edition)
- [ ] All dependencies pinned under `[workspace.dependencies]`
  [verify](tests/loom-test.sh::test_workspace_deps_pinned)
- [ ] All crates declare `[lints] workspace = true`
  [verify](tests/loom-test.sh::test_workspace_lints)
- [ ] No `types.rs` or `error.rs` files at crate roots
  [verify](tests/loom-test.sh::test_nested_module_structure)
- [ ] Domain identifiers use newtypes (BeadId, SpecLabel, MoleculeId, etc.)
  [verify](tests/loom-test.sh::test_newtypes_for_identifiers)
- [ ] No `unwrap()`, `todo!()`, `panic!()`, `unimplemented!()` in non-test code
  [verify](tests/loom-test.sh::test_no_panics_in_production)
- [ ] No `#[allow(dead_code)]` in non-test code
  [verify](tests/loom-test.sh::test_no_allow_dead_code)
- [ ] No `derive(From)` or `derive(Into)` on newtype structs
  [verify](tests/loom-test.sh::test_no_derive_from_on_newtypes)

### Templates

- [ ] All Ralph templates ported to Askama format
  [verify](tests/loom-test.sh::test_askama_templates_compile)
- [ ] Each template has a typed context struct with all required variables
  [verify](tests/loom-test.sh::test_template_context_structs)
- [ ] Templates compile at build time (missing variables are compile errors)
  [verify](tests/loom-test.sh::test_template_compile_time_check)
- [ ] Rendered output matches Ralph's bash-rendered output for identical inputs
  [verify](tests/loom-test.sh::test_template_output_parity)
- [ ] Partials included via Askama's `{% include %}` mechanism
  [verify](tests/loom-test.sh::test_template_partials)

### Process architecture

- [ ] Loom never invokes `podman run` directly (grep `loom/crates/` for
      `podman` finds only documentation references)
  [verify](tests/loom-test.sh::test_loom_does_not_invoke_podman)
- [ ] `wrapix spawn --spawn-config <file> --stdio` accepts a JSON config,
      reuses container construction from existing `wrapix run`, omits TTY
  [verify](tests/loom-test.sh::test_wrapix_spawn_subcommand)
- [ ] `SpawnConfig` JSON shape is stable: serialization round-trip preserves
      all fields and key names, including the `image_ref` and `image_source`
      fields
  [verify](tests/loom-test.sh::test_spawn_config_json_stability)
- [ ] `wrapix spawn` runs `podman load` from `image_source` (a Nix store
      path) before invoking podman with `image_ref` as the ref; the load is
      idempotent on the image's hash tag
  [verify](tests/loom-test.sh::test_wrapix_spawn_loads_image_source)
- [ ] Per-bead profile selection: two beads with different profile labels
      result in two `wrapix spawn` invocations with different `image_ref`
      and `image_source`
  [verify](tests/loom-test.sh::test_per_bead_profile_spawn)
- [ ] Loom reads `LOOM_PROFILES_MANIFEST` at startup and parses it into
      `BTreeMap<ProfileName, ImageEntry>`; missing env var or missing file
      errors before any bead spawn
  [verify](tests/loom-test.sh::test_profiles_manifest_required)
- [ ] A bead with `profile:X` where `X` is not in the manifest fails with a
      typed `ProfileError::UnknownProfile` naming the missing profile
  [verify](tests/loom-test.sh::test_unknown_profile_errors)
- [ ] `--profile` CLI override takes precedence over bead labels
  [verify](tests/loom-test.sh::test_profile_cli_override)
- [ ] `loom plan` shells out to interactive `wrapix run` (TTY attached); does
      not capture stdio for JSONL
  [verify](tests/loom-test.sh::test_plan_uses_interactive_wrapix_run)

### Concurrency & locking

- [ ] Spec-scoped mutating commands acquire `<label>.lock` and release on
      process exit
  [verify](tests/loom-test.sh::test_per_spec_lock_acquired)
- [ ] Two mutating commands on the same spec serialize: the second waits up
      to 5s, then errors clearly
  [verify](tests/loom-test.sh::test_per_spec_lock_serializes)
- [ ] Two mutating commands on *different* specs run concurrently (no
      blocking)
  [verify](tests/loom-test.sh::test_cross_spec_no_blocking)
- [ ] Read-only commands (`status`, `logs`, `spec`) acquire no lock and run
      during an active `loom run`
  [verify](tests/loom-test.sh::test_readonly_commands_unblocked)
- [ ] `loom init` and `loom init --rebuild` acquire the workspace lock
      and error immediately if any per-spec lock is held
  [verify](tests/loom-test.sh::test_init_workspace_lock)
- [ ] Crashed loom process leaves no stale lock (kernel releases flock on
      exit; new invocation acquires immediately)
  [verify](tests/loom-test.sh::test_crash_releases_lock)
- [ ] Lock files live under `$XDG_STATE_HOME/loom/locks/<workspace-
      basename>/` (default `~/.local/state/loom/locks/<basename>/`); no
      lock files are created inside the workspace bind-mount
  [verify](tests/loom-test.sh::test_locks_outside_workspace)
- [ ] Removing the lock file from inside the bead container does not
      break mutual exclusion on the host (locks live outside the
      bind-mount; agent has no path to them)
  [verify](tests/loom-test.sh::test_container_cannot_rm_host_lock)
- [ ] Driver sets `LOOM_INSIDE=1` in every bead container's env via the
      `SpawnConfig.env` allowlist
  [verify](tests/loom-test.sh::test_loom_inside_env_set)
- [ ] With `LOOM_INSIDE=1`, mutating subcommands (`run`, `init`, `plan`,
      `check`, `todo`, `msg`, `use`) refuse with a clear error
  [verify](tests/loom-test.sh::test_nested_loom_guard_refuses)
- [ ] With `LOOM_INSIDE=1`, read-only subcommands (`status`, `logs`,
      `spec`) still run normally
  [verify](tests/loom-test.sh::test_nested_loom_guard_allows_readonly)

### Run UX & logging

- [ ] Default terminal output prints one header line per bead and one short
      line per tool call (no streamed assistant text)
  [verify](tests/loom-test.sh::test_run_default_output_shape)
- [ ] `--verbose` / `-v` enables live assistant text streaming
  [verify](tests/loom-test.sh::test_run_verbose_streams_text)
- [ ] Full raw JSONL event stream is written to
      `.wrapix/loom/logs/<spec-label>/<bead-id>-<timestamp>.jsonl` for every
      bead spawn, regardless of terminal verbosity
  [verify](tests/loom-test.sh::test_run_writes_per_bead_jsonl_log)
- [ ] Log path is logged at `info!` when the spawn starts
  [verify](tests/loom-test.sh::test_run_logs_log_path)
- [ ] With `--parallel N > 1`, each bead writes to its own file (no
      interleaving in a single log)
  [verify](tests/loom-test.sh::test_parallel_logs_are_per_bead)
- [ ] Terminal renderer and log writer consume the same `AgentEvent` stream
      (single channel, not two pipelines)
  [verify](tests/loom-test.sh::test_run_single_event_channel)
- [ ] On `loom run` startup, log files older than `[logs] retention_days`
      (default 14) are deleted; recent logs are preserved
  [verify](tests/loom-test.sh::test_log_retention_sweep)
- [ ] `[logs] retention_days = 0` disables sweeping (no files deleted)
  [verify](tests/loom-test.sh::test_log_retention_disabled)
- [ ] Sweep failures (permission denied, in-use file) do not abort the run
  [verify](tests/loom-test.sh::test_log_retention_failure_tolerance)

### Worktree parallelism

- [ ] `loom run --parallel 1` (default) does not create a worktree and works
      on the driver branch directly
  [verify](tests/loom-test.sh::test_parallel_one_no_worktree)
- [ ] `loom run --parallel N` (N > 1) creates one worktree per dispatched bead
      under `.wrapix/worktree/<label>/<bead-id>/`
  [verify](tests/loom-test.sh::test_parallel_creates_worktrees)
- [ ] Each worktree spawns its own `wrapix spawn` and the spawns run
      concurrently (overlapping wall-clock)
  [verify](tests/loom-test.sh::test_parallel_concurrent_spawns)
- [ ] Successful bead branches are merged back to the driver branch after
      the batch completes
  [verify](tests/loom-test.sh::test_parallel_merge_back)
- [ ] On worker failure, the bead worktree branch is cleaned up and the bead
      is queued for retry per the retry policy
  [verify](tests/loom-test.sh::test_parallel_failure_cleanup)
- [ ] On merge conflict, the worktree is preserved and the bead is marked
      failed (not silently overwritten)
  [verify](tests/loom-test.sh::test_parallel_conflict_preserves_worktree)
- [ ] `GitClient` is the only module that imports `gix` or invokes the `git`
      CLI; callers see typed Rust methods
  [verify](tests/loom-test.sh::test_git_client_encapsulation)

### Workflow commands

- [ ] `loom plan -n <label>` spawns container with base profile, runs spec interview
  [verify](tests/loom-test.sh::test_plan_new)
- [ ] `loom plan -u <label>` updates existing spec with anchor/sibling support
  [verify](tests/loom-test.sh::test_plan_update)
- [ ] `loom todo` implements four-tier detection with per-spec cursor fan-out
  [verify](tests/loom-test.sh::test_todo_tier_detection)
- [ ] `loom run` continuous mode processes beads until molecule complete
  [verify](tests/loom-test.sh::test_run_continuous)
- [ ] `loom run --once` processes single bead then exits
  [verify](tests/loom-test.sh::test_run_once)
- [ ] `loom run --parallel N` (alias `-p N`) accepts a positive integer; non-
      positive or non-integer values fail with a clear error
  [verify](tests/loom-test.sh::test_run_parallel_flag_validation)
- [ ] `loom run` reads profile from bead label and spawns correct container
  [verify](tests/loom-test.sh::test_run_profile_selection)
- [ ] `loom run` retries failed beads with previous error context
  [verify](tests/loom-test.sh::test_run_retry_with_context)
- [ ] `loom run` execs `loom check` on molecule completion
  [verify](tests/loom-test.sh::test_run_execs_check)
- [ ] `loom check` implements push gate (push only on clean completion)
  [verify](tests/loom-test.sh::test_check_push_gate)
- [ ] `loom check` auto-iterates on fix-up beads (up to max iterations)
  [verify](tests/loom-test.sh::test_check_auto_iterate)
- [ ] `loom msg` lists outstanding `loom:blocked` and `loom:clarify` beads
  [verify](tests/loom-test.sh::test_msg_list)
- [ ] `loom msg -a` handles fast-reply (option selection for `loom:clarify`,
      free-form for `loom:blocked`)
  [verify](tests/loom-test.sh::test_msg_fast_reply)
- [ ] `loom spec` queries spec annotations (verify/judge)
  [verify](tests/loom-test.sh::test_spec_query)
- [ ] `loom spec --deps` scans verify/judge test files in the active spec
      and prints required nixpkgs (port of `ralph sync --deps`)
  [verify](tests/loom-test.sh::test_spec_deps)

### Verdict gate

- [ ] After every agent phase, `loom check` evaluates the result against
      the verdict-gate decision table; mechanical signals (marker,
      bd-closed, diff) make no LLM call
  [verify](tests/loom-test.sh::test_verdict_gate_mechanical_signals)
- [ ] `LOOM_BLOCKED` agent marker → bead transitions to `[blocked]`,
      recovery loop is skipped
  [verify](tests/loom-test.sh::test_gate_loom_blocked_marker)
- [ ] `LOOM_CLARIFY` agent marker → bead transitions to `[clarify]`,
      recovery loop is skipped
  [verify](tests/loom-test.sh::test_gate_loom_clarify_marker)
- [ ] No marker emitted → recovery with cause `swallowed-marker`
  [verify](tests/loom-test.sh::test_gate_swallowed_marker)
- [ ] `LOOM_COMPLETE` + bead not bd-closed → recovery with cause
      `incomplete-signaling`
  [verify](tests/loom-test.sh::test_gate_incomplete_signaling)
- [ ] `LOOM_COMPLETE` + closed + empty diff → recovery with cause
      `zero-progress`
  [verify](tests/loom-test.sh::test_gate_zero_progress)
- [ ] `LOOM_NOOP` + closed + empty diff → review runs (legitimate no-op
      proceeds to semantic review rather than zero-progress)
  [verify](tests/loom-test.sh::test_gate_loom_noop_empty_diff)
- [ ] All `[verify]` scripts on the bead's success criteria run; none
      short-circuit each other; per-script pass/fail + stderr is captured
  [verify](tests/loom-test.sh::test_gate_runs_all_verify_scripts)
- [ ] One or more `[verify]` failures → recovery with cause
      `verify-fail`; `previous_failure` carries every failure (not just
      the first), with a 4000-char budget split across them
  [verify](tests/loom-test.sh::test_gate_verify_fail_collects_all)
- [ ] Review (LLM step) runs regardless of `[verify]` result; on
      verify-fail, review's flag reasoning is appended to
      `previous_failure` under `Review notes:`
  [verify](tests/loom-test.sh::test_review_runs_on_verify_fail)
- [ ] Review's primary concern is live-path coverage: at least one
      `[verify]` on the bead must exercise the live path (same binary,
      same argv shape, same env). All-mock `[verify]` sets are flagged
  [judge](tests/judges/loom.sh::judge_live_path_coverage)
- [ ] Review flags mocks that stand in for the very thing the test
      claims to test (e.g. mocking the agent backend in an
      agent-integration test)
  [judge](tests/judges/loom.sh::judge_mock_discipline)
- [ ] Review's secondary concerns are scope appropriateness and
      `[judge]` rubric satisfaction
  [verify](tests/loom-test.sh::test_review_inputs_include_judge_rubrics)
- [ ] Review flag → recovery with cause `review-flag`; the flag detail
      names which concern triggered (live-path / mock / scope / judge)
  [verify](tests/loom-test.sh::test_gate_review_flag_names_concern)
- [ ] Recovery iter < `[loop] max_iterations` (default 3) → spawns
      fix-up bead OR retries the bead with prior failure context
  [verify](tests/loom-test.sh::test_recovery_under_max)
- [ ] Recovery iter ≥ max_iterations → applies `loom:blocked` with cause
      in `bd update --notes`
  [verify](tests/loom-test.sh::test_recovery_exhaustion_applies_blocked)
- [ ] Iteration count is bead-level state and survives `retry →
      [running]` round-trips
  [verify](tests/loom-test.sh::test_iteration_count_persists)
- [ ] Pre-flight infra failures (image load, container start) exit
      immediately as `loom:blocked` with cause `infra-preflight`; no retry
  [verify](tests/loom-test.sh::test_infra_preflight_fail_fast)
- [ ] Mid-session infra failures (agent process exit non-zero, container
      OOM, IO errors) get one free retry per `loom run`; second mid-
      session failure → `loom:blocked` with cause `infra-repeated`
  [verify](tests/loom-test.sh::test_infra_midsession_one_retry)
- [ ] Infra-retry counter is driver-memory only; resets on a fresh
      `loom run` invocation; does not consume `[loop] max_iterations`
  [verify](tests/loom-test.sh::test_infra_retry_counter_separate)
- [ ] `loom check` push gate refuses to push while any bead in the
      molecule carries `loom:blocked` or `loom:clarify`
  [verify](tests/loom-test.sh::test_push_gate_refuses_unresolved)

### Auxiliary commands

- [ ] `loom init` creates `.wrapix/loom/config.toml` and `.wrapix/loom/state.db`
      with the default schema
  [verify](tests/loom-test.sh::test_init_creates_state)
- [ ] `loom init --rebuild` repopulates the state DB from `specs/*.md`
      and active beads
  [verify](tests/loom-test.sh::test_init_rebuild)
- [ ] `loom status` prints active spec, current molecule, iteration count
      from the state DB
  [verify](tests/loom-test.sh::test_status_command)
- [ ] `loom use <label>` sets `current_spec` in the state DB; round-trips
      with `loom status`
  [verify](tests/loom-test.sh::test_use_command)
- [ ] `loom logs` tails the most recent JSONL log under
      `.wrapix/loom/logs/`; `--bead <id>` selects a specific bead's log
  [verify](tests/loom-test.sh::test_logs_command)
- [ ] No `loom sync` / `loom tune` commands exist (compiled templates make
      them unnecessary)
  [verify](tests/loom-test.sh::test_no_sync_or_tune_command)

### State database

- [ ] `StateDb::open` creates tables on first open
  [verify](tests/loom-test.sh::test_state_db_init)
- [ ] `StateDb::rebuild` populates from spec files and active beads
  [verify](tests/loom-test.sh::test_state_db_rebuild)
- [ ] `StateDb::rebuild` parses each spec's `## Companions` section and
      writes one `companions` row per listed path; specs without the
      section contribute zero rows (not an error)
  [verify](tests/loom-test.sh::test_state_db_rebuild_companions)
- [ ] `StateDb::rebuild` resets iteration counters to 0
  [verify](tests/loom-test.sh::test_state_db_rebuild_resets_counters)
- [ ] `current_spec` / `set_current_spec` round-trips correctly
  [verify](tests/loom-test.sh::test_state_current_spec)
- [ ] `increment_iteration` returns updated count
  [verify](tests/loom-test.sh::test_state_increment_iteration)
- [ ] Corrupted DB file → `loom init --rebuild` recovers
  [verify](tests/loom-test.sh::test_state_corruption_recovery)
- [ ] Per-spec `loom todo` cursor round-trips through the `meta` table —
      `set_todo_cursor` overwrites prior values (cursor advances forward)
      and per-label namespacing keeps distinct specs disjoint
  [verify](tests/loom-test.sh::test_state_todo_cursor)
- [ ] `loom todo` advances the cursor only when the session emitted a
      `LOOM_COMPLETE` or `LOOM_NOOP` marker AND `exit_code == 0`; any
      other terminal state (no marker, nonzero exit, `LOOM_BLOCKED`,
      `LOOM_CLARIFY`) leaves the cursor untouched
  [verify](tests/loom-test.sh::test_todo_cursor_advance_requires_marker)
- [ ] `loom plan -n <label>` inserts a `specs` row and populates
      `implementation_notes` from the interview
  [verify](tests/loom-test.sh::test_plan_new_writes_implementation_notes)
- [ ] `loom plan -u <label>` reads the existing `implementation_notes`,
      and writes back a merged array (interview-driven keep/drop/add —
      not blind append, not blind replace)
  [judge](tests/judges/loom.sh::judge_plan_update_merges_notes)
- [ ] `loom todo` reads `implementation_notes` from the anchor row,
      renders them into each new bead body, then clears the column
      (`UPDATE … SET implementation_notes = NULL`); the row itself stays
  [verify](tests/loom-test.sh::test_todo_consumes_and_clears_notes)
- [ ] Routine commands never DELETE a `specs` row; row removal happens
      only via `loom init --rebuild`
  [verify](tests/loom-test.sh::test_routine_commands_never_delete_spec_row)

### Compaction recovery

- [ ] At session start, `.wrapix/loom/scratch/<key>/` contains
      `prompt.txt`, `scratch.md`, `repin.sh` for every phase command
      (plan, todo, run, check, msg)
  [verify](tests/loom-test.sh::test_scratch_dir_created)
- [ ] `<key>` is the spec label for plan/todo phases and the bead ID for
      run/check/msg phases
  [verify](tests/loom-test.sh::test_scratch_key_naming)
- [ ] Running `repin.sh` emits a valid `SessionStart[compact]` JSON
      envelope containing banner + `prompt.txt` + `scratch.md` contents
  [verify](tests/loom-test.sh::test_repin_envelope)
- [ ] `claude-settings.json` registers `repin.sh` under
      `SessionStart[matcher: compact]`
  [verify](tests/loom-test.sh::test_repin_hook_registered)
- [ ] On session end (success or failure), the per-key scratch directory
      is removed
  [verify](tests/loom-test.sh::test_scratch_dir_cleanup)
- [ ] Two parallel `loom run` workers on different beads use independent
      scratch directories and do not collide
  [verify](tests/loom-test.sh::test_parallel_scratch_isolation)
- [ ] `partial/scratchpad.md` instructs the agent that the scratchpad is
      agent-lifecycle-only and points at durable destinations for
      long-term records
  [judge](tests/judges/loom.sh::test_scratchpad_partial_clarity)

### Beads CLI wrapper

- [ ] `bd show` output parsed into typed `Bead` struct
  [verify](tests/loom-test.sh::test_bd_show_parsing)
- [ ] `bd list` output parsed with label and status filtering
  [verify](tests/loom-test.sh::test_bd_list_parsing)
- [ ] `bd create` returns created bead ID
  [verify](tests/loom-test.sh::test_bd_create_returns_id)
- [ ] CLI errors mapped to typed error variants
  [verify](tests/loom-test.sh::test_bd_error_handling)

### Nix integration

- [ ] Loom binary builds via `nix build`
  [verify](tests/loom-test.sh::test_nix_build)
- [ ] Loom available in devShell alongside ralph
  [verify](tests/loom-test.sh::test_devshell_includes_loom)
- [ ] `cargo clippy` passes with workspace lints
  [verify](tests/loom-test.sh::test_clippy_clean)
- [ ] `cargo test` passes for all crates
  [verify](tests/loom-test.sh::test_cargo_test)

## Out of Scope

- **Deleting Ralph's bash scripts** — happens after Loom reaches parity, not in
  this spec.
- **Gas City integration** — Gas City is experimental and token-heavy. Loom
  does not need to integrate with or replace Gas City's agent management.
- **Agent backend implementations** — defined in [loom-agent.md](loom-agent.md).
- **Workflow semantics changes** — Loom reimplements existing behavior from
  ralph-loop.md and ralph-review.md. No new workflow features.
- **Parallelism beyond v1 parity** — `loom run --parallel N` matches ralph's
  worktree-per-bead model and feature set. New parallelism strategies
  (cross-spec, distributed, scheduler-aware) are future work.
- **Hidden specs (`-h` flag)** — Ralph's hidden-spec mode (spec lives in
  `.wrapix/ralph/state/<label>.md`, not committed, single-spec only) is
  deliberately not ported. It's a degraded mode (no siblings, no
  companions, no fan-out) whose use case — keeping a spec out of git —
  is covered by `.git/info/exclude` on `specs/<label>.md`. Removing the
  flag eliminates an asymmetric branch from `plan`/`todo`/`run`
  path-resolution. Reintroducing it later is a non-breaking additive
  change if the workflow asks for it.
- **Per-project template customization** — Loom templates are Askama,
  compiled into the binary. `ralph sync` (template fetch/backup/diff)
  and `ralph tune` (AI-assisted template editing) have no v1 equivalent.
  Project-specific prompt tweaks happen via `pinned_context` and the
  per-spec implementation-notes mechanism, not by editing template
  source.
- **Observation daemon (`ralph watch`)** — polling monitor that spawns
  short-lived agent sessions to observe tmux/browser logs and create
  beads. Independent of the workflow phase set; deferred to a follow-up
  spec.
- **Session persistence across container restarts** — each container starts a
  fresh agent session.

## Configuration

Loom reads `.wrapix/loom/config.toml` — its own config file, independent of
Ralph's `.wrapix/ralph/config.nix`. Parsed natively via the `toml` crate into
a typed `LoomConfig` struct with `#[serde(default)]` on all fields so the file
can be empty or absent (defaults apply).

```toml
pinned_context = "docs/README.md"

[beads]
priority = 2
default_type = "task"

[loop]
max_iterations = 3
max_retries = 2
max_reviews = 2

[logs]
# Delete log files under .wrapix/loom/logs/ older than this many days on
# `loom run` startup. 0 disables sweeping (keep forever).
retention_days = 14

# Per-phase config. Resolution for any field: [phase.<name>] →
# [phase.default] → built-in. `loom run` reads its profile from the
# bead's `profile:X` label first, then [phase.run] / [phase.default];
# the `--profile` CLI flag overrides everything.
[phase.default]
profile = "base"
agent.backend = "claude"

# [phase.todo]
# profile = "rust"
# agent.backend = "pi"
# agent.provider = "deepseek"
# agent.model_id = "deepseek-v3"
#
# [phase.check]
# agent.backend = "claude"

[claude]
# Agent-runtime settings, applied wherever claude is selected. Seconds to
# wait for clean exit after `result` before SIGTERM (shutdown watchdog).
post_result_grace_secs = 5

# Backend-agnostic liveness knobs. `handshake_timeout_secs` bounds the pi
# startup probe + optional set_model response — a non-responsive launcher
# fails fast with `HandshakeTimeout` instead of hanging. `stall_warn_secs`
# emits a `warn!` every N seconds of agent silence on the run loop without
# aborting; claude can legitimately think for minutes, so this is a
# heartbeat, not a deadline. Defaults: 30s / 60s.
# handshake_timeout_secs = 30
# stall_warn_secs = 60

[security]
# Tool names to deny when claude sends control_request. Claude-only —
# pi has no host-side permission flow (tools execute internally, no
# control_request analog). Empty by default; the container sandbox is
# the trust boundary.
# denied_tools = ["SomeNewHostTool"]
```

Defaults match Ralph's so users can transition without configuring Loom
separately. Settings Ralph has that Loom doesn't need (output display,
hooks, watch, failure patterns) are omitted — Loom handles those concerns
in Rust code, not config.
