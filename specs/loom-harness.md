# Loom Harness

Rust workspace, build system, workspace lint config, process architecture,
state store, and command-set platform for the Loom agent driver.

## Problem Statement

Loom is a Rust binary that owns a complete spec-driven workflow:
spec interview (plan), spec-to-beads decomposition (todo), per-bead
agent dispatch (run), deterministic and LLM-judged review (check +
review), and human clarification (msg). The binary holds the
workflow's state in typed domain objects, parses agent protocols
against typed schemas, and renders templates with compile-time
variable validation.

This spec covers the platform: crate structure, Rust conventions,
Nix integration, SQLite state store, beads CLI wrapper, process
architecture, and recovery mechanics. The Askama template engine,
partials inventory, per-phase pinning policy, and snapshot-test
contract live in [loom-templates.md](loom-templates.md). The agent
abstraction layer (pi-mono and Claude Code backends, container
communication, backend selection) lives in
[loom-agent.md](loom-agent.md). The gate (rubric, invariants, lanes,
stages) lives in [loom-gate.md](loom-gate.md). Workflow
semantics — what each `loom plan` / `loom todo` / `loom run` /
`loom check` / `loom review` / `loom msg` command does — are
defined in this spec's Functional section and the Msg Modes /
Verdict Gate sections below.

## Requirements

### Functional

1. **Command set** — workflow commands implementing the loom phases,
   plus a small set of auxiliary state / log management commands:

   **Workflow commands (driver phases):**
   - `loom plan` — spec interview (interactive agent session); flags
     `-n <label>` for a new spec and `-u <label>` for updating an existing
     one. No hidden-spec flag: scratch / private specs are kept out of
     git via `.git/info/exclude` rather than a separate spec home.
   - `loom todo` — spec-to-beads decomposition
   - `loom run` — execute beads in loop (continuous or `--once`). The
     loop progresses on every bead `bd ready` returns; beads carrying
     `loom:blocked` or `loom:clarify` are filtered out by `bd ready`
     and skipped on subsequent ticks. Other non-dependent beads in
     the molecule continue running — a single clarified or blocked
     bead does not halt the loop. Under `--parallel N`, a clarify or
     block on one of the N concurrent beads does not cancel the
     others; they each run to completion, and only re-dispatch of the
     clarified / blocked bead is skipped. On molecule completion,
     execs `loom check` then `loom review` then fires the push gate.
   - `loom check` — deterministic audits (runs `[verify]` scripts,
     style linters, and the `--check=<name>` sub-audits — `criteria`,
     `removals`, `infrastructure`, `cross-spec` per TST-5..9). Scope
     flags: `--bead <id>`, `--diff <range>`, `--tree`. Subsumes the
     former `loom doctor` command. See
     [loom-gate.md](loom-gate.md) for the gate's semantics.
   - `loom review` — LLM-judged review rubric (conformance trace,
     contract closure, verifier honesty, mock discipline, invariant
     clashes). Scope flags: `--bead <id>`, `--diff <range>`, `--tree`.
     Reads `loom check` results as input; runs only after `loom
     check` passes the same scope. See [loom-gate.md](loom-gate.md)
     for the rubric.
   - `loom msg` — clarify resolution
   - `loom spec` — query spec annotations; supports `--deps` to print
     nixpkgs required by the spec's `[verify]` / `[judge]` test files

   **Auxiliary commands (state / log management):**
   - `loom init` — create `.wrapix/loom/` config + state DB; `--rebuild`
     repopulates the state DB from `specs/*.md` and active beads
   - `loom status` — print active spec, current molecule, iteration count
     (trivial state DB query)
   - `loom use <label>` — set `current_spec` in the state DB; `loom status`
     reads it back
   - `loom logs` — pretty-render a bead's JSONL log under
     `.wrapix/loom/logs/` via the same `AgentEvent` renderer used by
     `loom run`. Full flag set in [Logs UX](#logs-ux).

2. **Compiled templates** — Askama engine, per-phase templates, partials,
   and per-phase pinning policy live in
   [loom-templates.md](loom-templates.md). The crate that builds them
   (`loom-templates`) is one of the seven enumerated below; everything
   inside it is owned by that spec.
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
   driver branch sequentially. Default parallelism is 1 (sequential).
7. **Retry with context** — on in-session worker failure, retries with the
   prior error output injected as the `previous_failure` template variable.
   Configurable max retries per bead (default 2). After in-session retries
   exhaust, the phase ends; the verdict is delegated to the
   [Verdict Gate](#verdict-gate).
8. **Verdict gate per phase** — `loom check` (deterministic) followed
   by `loom review` (LLM) evaluates each phase's result before the
   bead's state can advance. See [Verdict Gate](#verdict-gate) for the
   execution layer (decision table, recovery mechanics, markers, labels)
   and [loom-gate.md](loom-gate.md) for the review rubric.
   Driver-detected gate failures enter a bounded recovery loop;
   agent self-reports (`LOOM_BLOCKED` / `LOOM_CLARIFY`) escalate
   directly to the human via `loom msg`.
9. **Push gate** — push fires only when every bead in the molecule has
   reached `[done]` (no `loom:blocked` or `loom:clarify` outstanding)
   *and* both `loom check` and `loom review` have passed for the
   final iteration. Auto-iterates on fix-up beads up to max
   iterations before refusing.
10. **Beads via shared Dolt socket** — every container has the host's
    `wrapix-beads` Dolt server bind-mounted at
    `/workspace/.wrapix/dolt.sock`; in-container `bd` writes go straight to
    the authoritative state. No per-bead `bd dolt push/pull` handoff. Loom
    on the host reads the same state through the same socket. The legacy
    `.beads/issues.jsonl` path is not used — beads no longer supports it.
11. **Spec resolution** — `--spec <name>` flag or fallback to the
    `current_spec` key in the state database.

### Non-Functional

1. **Rust style** — all loom crates follow
   [`docs/style-rules.md`](../docs/style-rules.md) RS-1..RS-16: workspace
   deps, workspace-root lint config (`[workspace.lints.*]` + `clippy.toml`),
   `thiserror` + `displaydoc` error enums, nested module structure,
   parse-don't-validate at boundaries, newtypes for IDs, no
   `derive(From)`/`derive(Into)` on newtypes, no panicking macros (incl.
   `unreachable!()`), no silent error swallows, no sentinel newtypes,
   `Default` only when the zero-value is safe, no test-fixture shape in
   production trait impls, structured `tracing` logging, avoid stutter
   naming. Per-site `#[expect(...)]` requires a substantive
   `reason = "..."` (RS-3).
2. **Required newtypes** — `BeadId`, `SpecLabel`, `MoleculeId`,
   `ProfileName` for domain identifiers; `SessionId`, `ToolCallId`,
   `RequestId` for protocol identifiers. No bare `String` for typed IDs.
   `AgentKind` is an enum (`Pi`, `Claude`), not a newtype.
3. **Nix integration** — built via `wrapix.profiles.rust.buildPackage`
   (crane-backed; see [profiles.md — Rust package builder](profiles.md#rust-profile)).
   `packages.loom` consumes `.bin` so devshell rebuilds skip the clippy/nextest
   passes; those land as separate `loom-clippy` / `loom-nextest` entries in
   `nix flake check`. Binary is included in the devShell.

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
`wrapix spawn` invocations) and per-bead container spawn cost (~1-2s
on podman) is dominated by agent runtime for typical bead sizes
(minutes of agent work). The alternative — one long-lived container
sharing one agent across beads — was rejected because it conflicts
with per-bead profile selection and with the trust-boundary split
between host orchestrator and sandboxed agent.

### Worktree Parallelism

`loom run --parallel N`:

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
and merge operations go through a typed `GitClient` in `loom-driver`. The
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
`BTreeMap<ProfileName, ImageEntry>` in `loom-driver`.

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
| Read-only | `status`, `logs` (incl. `-f` follow), `spec` | none |
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
Machine consumers (CI harnesses, hosted runners feeding web frontends via
SSE, log analyzers) get a separate well-defined channel via `--json`.

**Renderer architecture.** A single `Renderer` trait in `loom-render`
consumes `AgentEvent` values; one impl is selected at startup based on
flags + TTY detection.

| Mode | Selected when | Output |
|------|---------------|--------|
| `Pretty` (default) | TTY, no `--plain`, no `--json`, no `--raw` | colored, glyphs, indented tool bodies, `imara-diff` for `Edit`/`Write`, OSC 8 hyperlinks where supported |
| `Plain` | non-TTY (pipe / redirect), `NO_COLOR`, or `--plain` | ASCII glyphs, no color, no OSC 8; same shape as `Pretty` minus decoration |
| `Json` | `--json` | one pretty-printed JSON object per line; pure data, zero ANSI |
| `Raw` | `--raw` | passthrough of the original JSONL bytes; no parsing, no formatting |

`loom logs` reuses the same trait + impls — replay and live render through
identical code paths. No second formatter, no drift.

**Default `Pretty` output** (one bead, single sequential run):

```
▸ wx-abc123  Implement parser                       [profile:rust]
  Read    src/parser/mod.rs                                       40 lines
  Edit    src/parser/mod.rs   +3 -1                                  diff↓
       enum Token {
           Number(i64),
    -      Other,
    +      Identifier(String),
    +      Other,
       }
  Bash    cargo test --lib                                          12.4s ✓
       running 14 tests
       test parser::test_identifier ... ok
       test result: ok. 14 passed; 0 failed
  ◇ All tests pass. Closing.
  ✓ done   3 tool calls, 47s
    tokens:  18.2k in · 3.4k out      cost: $0.073
    log:     .wrapix/loom/logs/loom-harness/wx-abc123-2026-05-08T14-32-09.jsonl
```

**Per-tool summary cells.** Every builtin gets a tailored one-line summary
so the eye learns the shape and `loom logs -b X | grep Edit` stays useful.
The renderer dispatches on `tool_name`; unknown tools fall through to a
generic `<name>  <truncated args>` row.

| Tool | Summary cell | Body (default) | Body (`-v`) |
|------|--------------|----------------|-------------|
| `Read` | `<repo-relative-path>:<line-range>   <N> lines` | hidden | full file slice |
| `Edit` | `<repo-relative-path>   +<add> -<del>   diff↓` | `imara-diff` unified diff, capped at 10 lines | full diff |
| `Write` | `<repo-relative-path>   +<lines>   new file` | first 10 lines | full content |
| `Grep` | `"<pattern>" in <path>   <N> files` | first 10 matches | full matches |
| `Glob` | `"<pattern>" in <path>   <N> files` | first 10 paths | full list |
| `Bash` | `<command-truncated>   <duration> <✓|✗ exit=N>` | first 10 stdout/stderr lines | full output |
| `WebFetch` | `<url>   <bytes> <duration> <✓|✗>` | first 10 lines of response | full body |
| `WebSearch` | `"<query>"   <N> results` | first 10 result titles | full list |
| `Task` | `<description>   [agent:<subagent-type>]   <duration> <✓|✗>` | nested events at deeper indent | nested events at deeper indent |

**Truncation policy.** Tool bodies cap at **10 lines or 2 KB**, whichever
hits first. The cap line names the recovery: `[N more lines — loom logs -b
<bead-id> --tool <tool-call-id>]`. Single body lines wider than the
terminal hard-truncate at terminal width with `…`; bash output that's
truncated upstream (`bashExecution.truncated` from pi-mono) surfaces the
flag verbatim, never re-truncated.

**Subagent (`Task`) tool nesting.** When the agent calls `Task`, the
nested session's events render under the parent with two extra indent
levels so the call structure is visible. Nested `tool_call` /
`tool_result` events carry a `parent_tool_call_id` field; the renderer
uses it to indent + scope:

```
  Task    Branch ship-readiness audit  [agent:general-purpose]
      Read    src/lib.rs                                          200 lines
      Bash    git status                                            ✓
      Bash    git log --oneline -20                                 ✓
      ◇ Branch is 3 commits ahead of main, all tests pass.
      ✓ done   3 tool calls, 12s
```

**Driver-emitted events.** Loom itself produces events the user cares
about — retry decisions, gate verdicts, push refusal — and they ride the
same channel as agent events with `source: "driver"`. The renderer marks
them with `→` so the eye separates "what loom did" from "what the agent
did":

```
  Bash    cargo test --lib                                          8.1s ✗ exit=1
  → driver  verdict gate → recovery (cause: verify-fail, attempt 2/3)
  → driver  dispatching retry with previous_failure context
▸ wx-abc123  Implement parser                       [profile:rust]   attempt 2/3
  ...
```

Driver events at the molecule level (push gate walking the molecule, push
refusal, push success) render between bead blocks at zero indent.

**In-place running indicator.** While a tool is in flight, the renderer
keeps the cursor on the tool's header line and updates an inline duration
counter via `\r` + clear-to-EOL until the result arrives:

```
  Bash    cargo test --lib                                running... 4.2s
```

When the result arrives, the same line is rewritten with the final form
(duration, exit, ✓/✗) and the body prints below. Pure `\r`, no alt-screen,
no widget framework. Auto-suppresses in non-TTY (`Plain`/`Json`/`Raw`)
where carriage-return semantics don't compose with line-buffered consumers.

**`-v` / `--verbose` semantics.** One flag for "show me everything":
disables tool-body truncation, streams `text_delta`/`thinking_delta`
events live (one render per delta rather than waiting for `text_end`), and
shows `thinking` blocks (`◆`) which are hidden by default. No finer-grained
sub-flags in v1 — collect feedback before adding `--no-truncate` or
`--show-thinking` separately.

**Path normalization.** Tool args carrying absolute paths
(`/workspace/loom/crates/...`) render as repo-relative
(`loom/crates/...`). The agent uses absolute internally; normalization is
display-only. Both grep-friendly and shorter.

**Terminal hyperlinks (OSC 8).** Paths and URLs in summary cells are
emitted as OSC 8 hyperlinks when `TERM_PROGRAM` / `terminfo` indicates
support (iTerm2, Kitty, WezTerm, recent VS Code, Alacritty 0.13+, GNOME
Terminal). Cmd-click on `src/parser/mod.rs:42` jumps the user's editor;
cmd-click on a `WebFetch` URL opens browser. Auto-degrades silently to
plain text on unsupported terminals — no fallback warning, no separate
flag.

**Cancellation.** Ctrl-C / SIGINT during a run produces a clean closing
block; the in-place running indicator is collapsed, the partial diff is
captured, and the closing line surfaces `⚠ interrupted`:

```
  Bash    cargo build --release                                running... 8.1s
  ⚠ interrupted by user (SIGINT)
  ✗ cancelled  6 tool calls, 18s   partial diff: +42 -3 (uncommitted)
    log:     .wrapix/loom/logs/loom-harness/wx-abc123-...jsonl
```

A panic hook + `tokio::signal` handler ensure the in-place region is
cleared on every exit path — Codex's alt-screen restore bug (issue
#21345) is the cautionary tale, mitigated here by not using alt-screen
at all.

**Color discipline.** Color signals status only — green `✓`, red `✗`,
yellow for retry/clarify, dim grey for `compaction`/`auto-retry` lines.
Tool output bodies stay plain (no syntax highlighting in v1) so
scrollback grep stays clean. Glyphs (`▸`/`◇`/`◆`/`⚑`/`⋯`/`→`) carry
status independently of color so `NO_COLOR` and colorblind users get the
same signal.

**Parallel runs.** With `--parallel N > 1`, every line carries a
`[bead-id]` prefix colorized to a stable hash-derived hue per bead so
interleaved output is attributable at a glance. Bead headers and closing
lines print atomically (no interleaving mid-line). The in-place running
indicator is **disabled** in parallel mode — multiple `\r`-updating
regions on the same terminal don't compose. Cost is borne; the gain (no
visual chaos) outweighs it.

**Closing summary line.** Every bead closes with a structured summary —
status glyph, tool count, duration, then on indented subsequent lines:

- `tokens:` input/output, summed across all turns
- `cost:` USD, summed
- `log:` resolved log path (OSC 8 hyperlinked when supported)
- on failure: `cause:` (verify-fail / review-flag / swallowed-marker / etc.)
- on cancellation: `partial diff:` size of uncommitted changes

**Log persistence.** Loom always writes the **full raw JSONL** event
stream for every bead to disk via a tee-style sink, regardless of
terminal verbosity. One file per bead spawn:

```
.wrapix/loom/logs/<spec-label>/<bead-id>-<utc-timestamp>.jsonl
```

Per-bead (not per-session) so parallel batches never interleave inside a
single file. The path is logged at `info!` when the spawn starts so users
can `tail -f` it. **Per-event flush** is mandatory — downstream
consumers (`tail -f`, file-watching SSE servers, CI ingest) see each
event promptly rather than at OS-buffer-flush cadence.

**Retention.** Logs are swept on `loom run` startup: any file under
`.wrapix/loom/logs/` whose mtime is older than `[logs] retention_days`
(default 14) is deleted. `retention_days = 0` disables sweeping (keep
forever). The sweep is best-effort and logged at `debug!` — failures to
delete (permission, in-use file) do not abort the run. Sweeping runs once
per `loom run` invocation, before any bead spawns; the cost is a single
directory walk.

The terminal renderer and the disk writer consume the same `AgentEvent`
stream — there's one channel, two subscribers, never two parallel
pipelines.

### Event Schema

`AgentEvent` is loom's typed event union and the **public contract**
between the producer (loom + agent backends) and downstream consumers
(terminal renderer, disk log, `--json` pipelines, hosted runners feeding
web frontends via SSE, third-party log tools). It lives in the
`loom-events` crate — the only crate a frontend, log analyzer, or SSE
bridge needs to depend on.

The wire shape is **flat tagged JSON** — one discriminator at the top
level, no nested `message_update { delta: { type: ... } }` envelopes.
A consumer dispatches with one `match` (Rust) or one `switch (event.kind)`
(TypeScript).

**Common envelope.** Every event carries the same seven structural fields
plus its variant-specific payload, all flat at the top level:

```json
{
  "kind": "tool_call",
  "bead_id": "wx-abc123",
  "molecule_id": "wx-mol-9",
  "iteration": 2,
  "source": "agent",
  "ts_ms": 1746715929123,
  "seq": 42,
  "tool_call_id": "toolu_01Pt",
  "tool": "Read",
  "params": { "file_path": "src/parser/mod.rs" },
  "parent_tool_call_id": null
}
```

| Field | Type | Purpose |
|-------|------|---------|
| `kind` | `string` | Discriminator — variant name, snake_case |
| `bead_id` | `string` | Per-bead routing; survives mid-stream join (SSE consumers, log analyzers) |
| `molecule_id` | `string` | Per-molecule grouping for push-gate / multi-bead UIs |
| `iteration` | `u32` | Bead's iteration counter (1-based; `1` on first attempt, `2` after first retry) |
| `source` | `"agent" \| "driver"` | Distinguishes agent activity from driver-emitted events |
| `ts_ms` | `i64` | Unix milliseconds UTC. Zoneless, half the bytes of RFC 3339, integer-comparable |
| `seq` | `u64` | Monotonic per-bead-spawn counter. SSE resume key: `Last-Event-ID: <bead_id>:<seq>` |

**Variants** (18, flat tagged enum, snake_case Rust + wire):

- **Lifecycle** — `agent_start`, `agent_end`, `turn_start`, `turn_end`,
  `session_complete`
- **Streaming** — `text_delta`, `text_end`, `thinking_delta`,
  `thinking_end`, `toolcall_delta`
- **Tools** — `tool_call`, `tool_result` (carries `is_error: bool`),
  `tool_progress`
- **Operational** — `compaction_start`, `compaction_end`, `auto_retry`,
  `error`
- **Driver catch-all** — `driver_event`

`agent_start` carries `schema_version: u32` (currently `1`), `title`,
`profile`, `spec_label`, `started_at_ms` (the bead-spawn anchor;
per-event `ts_ms` is sampled per emit), and
`parent_tool_call_id: Option<ToolCallId>` (set when this is a subagent
spawned by a `Task` tool — the field is what makes nested rendering
possible end-to-end).

`tool_call` carries `parent_tool_call_id: Option<ToolCallId>` synthesized
by the parser tracking the active `Task` call stack. Pi-mono and Claude
both lack this on the wire; loom synthesizes it so subagent nesting in
the renderer is one indent computation, not a state-machine inference.

`driver_event` is the **untyped catch-all** for loom-emitted events:

```json
{ "kind": "driver_event", "source": "driver",
  "driver_kind": "verdict_gate",
  "summary": "verdict gate → recovery (cause: verify-fail, attempt 2/3)",
  "payload": { "outcome": "recovery", "cause": "verify-fail", "attempt": 2 },
  "bead_id": "wx-abc123", "molecule_id": "wx-mol-9",
  "iteration": 2, "ts_ms": 1746715929123, "seq": 99 }
```

`driver_kind` (e.g. `verdict_gate`, `retry_dispatch`, `push_gate_walk`,
`push_gate_refuse`, `push_gate_clean`, `container_spawn`,
`container_oom`, `infra_failure`) is a free-form string. Adding new
driver event types is **additive on the wire** — no `schema_version`
bump required. The renderer dispatches on `driver_kind`; unknown kinds
fall through to a generic `→ <driver_kind>: <summary>` line. `summary`
is always present so even unknown driver events render meaningfully;
`payload` is a structured `serde_json::Value` for typed consumers.

**Schema versioning.**

- **Wire format** — the `schema_version` field on `agent_start`. Adding
  new top-level variants, new fields on existing variants, or new
  `driver_kind` values is minor (consumers ignore unknown variants /
  fields). Renaming, removing, or repurposing fields requires a major
  bump; consumers version-gate on the major.
- **Rust API** — the `loom-events` crate's semver tracks the same
  surface. Non-additive enum changes require a crate major bump. The
  two surfaces stay locked: the same `loom-events` version emits the
  same wire schema.

A consumer **must accept unknown event kinds gracefully** — drop them,
or render them as a generic `<kind>` line. Unknown variants are not an
error; they're the contract working across versions.

**Pi-mono and Claude adapters.** Pi-mono's `message_update` envelope is
flattened into top-level `text_delta` / `thinking_delta` / `toolcall_delta`
events at the parser layer; Claude's transcript schema maps to the same
flat variant set. See [loom-agent.md](loom-agent.md) for the per-backend
adapter contract.

**Type-level guarantees.** `AgentEvent` derives
`Serialize + Deserialize + Debug + Clone` — so `loom logs` reads its own
JSONL files back through the same enum it writes, and the renderer is
the same code path live or replay. No second formatter, no drift. ID
newtypes (`BeadId`, `MoleculeId`, `ToolCallId`) live in `loom-events`,
all `#[serde(transparent)]` over `String`.

**`loom-events` dependencies — three crates total.**
`serde` + `serde_json` + `thiserror`. No `chrono` (timestamps are `i64`
millis), no `ulid` (sequence is `u64`), no `uuid`. The whole crate is
the smallest surface area we can credibly hand a frontend bridge.

**SSE integration.** A pipeline runner that wants to broadcast a bead's
event stream over SSE: pull `loom-events`, tail the bead's JSONL log,
deserialize each line as `AgentEvent`, emit
`id: <bead_id>:<seq>\nevent: <kind>\ndata: <json>\n\n`. SSE clients
resume on disconnect via `Last-Event-ID`. Loom does not ship an SSE
server — `loom-events` is the integration boundary, the rest is the
pipeline runner's concern.

**Live-vs-replay distinction.** The renderer takes one bool at
construction (`live: bool`). When `live`, `tool_call` events without a
matching `tool_result` show the in-place running indicator (spinning
duration counter via `\r`). When `!live` (replay through `loom logs`),
events are already complete — the indicator is suppressed, durations
are computed from `ts_ms` deltas between paired `tool_call` and
`tool_result` events. Same code path, one bool.

**Renderer state.** The `Pretty` / `Plain` impls hold a
`HashMap<ToolCallId, PendingToolCall>` so a `tool_call` + `tool_result`
pair collapses into one rendered block with duration. The map is
unbounded in principle but bounded in practice by the agent's tool-call
concurrency (typically 1 in flight, ≤16 with parallel subagents). No
cap is enforced; if an agent leaves tool calls unmatched (crashed
mid-call) they linger until session end and render as partial blocks.

**Disk writer.** `LogSink` is a synchronous `BufWriter<File>` with
per-event flush. The flush is the contract: `tail -f` and SSE-via-file-
watcher consumers see each event at emit time, not at OS-buffer cadence.
The agent's IO is bound by the disk write+flush — measured at <100µs
per event on local SSD, well below per-token agent latency, so no
async channel or backpressure machinery is justified.

### Logs UX

`loom logs` replays or tails a saved log file via the same renderer used
by `loom run`. Reusing the renderer (rather than shipping a second
formatter) keeps live and replay output identical and prevents drift.

| Flag | Behavior |
|------|----------|
| (default) | Pretty-render the most recent bead's full log; exit at EOF |
| `-f` / `--follow` | Same renderer, tail-mode (block on EOF, like `tail -f`) |
| `-b` / `--bead <id>` | Select a specific bead instead of the most recent |
| `-v` / `--verbose` | Stream assistant text deltas (parity with `loom run -v`) |
| `--raw` | Emit raw JSONL bytes from the file, unparsed (for `jq` pipelines) |
| `--path` | Print the resolved log file path and exit; preserves today's `tail -f $(loom logs --path)` recipe |

`-f` and `--raw` compose: `loom logs -f --raw` tails raw JSONL, the
spiritual successor to today's `tail -f $(loom logs)` shorthand.
`--path` is mutually exclusive with `-f`, `-v`, and `--raw` — it
short-circuits to a path-only output before any rendering happens.
`-b` combines with everything (it just changes which file is selected).

**Empty-logs case.** Bare `loom logs` against an empty
`.wrapix/loom/logs/` prints a one-line message
(`No bead logs yet. Run 'loom run' to generate one.`) and exits 0 —
this is normal post-`loom init`, not an error. `--path` against an
empty logs directory exits non-zero with a clear error so scripts
relying on `$(loom logs --path)` fail loudly rather than expanding to
an empty string.

**No auto-follow.** Bare `loom logs` does **not** detect a still-running
bead and switch to follow mode automatically. Auto-detection (file
mtime, fd introspection) is brittle and surprising. Users who want
live tailing pass `-f` explicitly — matches the `tail` vs `tail -f`
mental model already in muscle memory.

### Verdict Gate

After every agent phase ends, the verdict gate evaluates the result
before the bead's state can advance. The gate runs in two passes:
`loom check` (deterministic — mechanical signals, `[verify]` scripts,
style linters) followed by `loom review` (LLM-judged rubric). The
review rubric, inputs, and concerns are defined in
[loom-gate.md](loom-gate.md); this section retains the execution
layer — the decision table, recovery mechanics, markers, labels, and
infra-failure handling.

Driver-detected failures enter a bounded recovery loop; agent
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

In the table above, `—` means the signal isn't inspected because an
earlier signal already determined the outcome (e.g. an agent self-report
short-circuits before review runs); `*` means any value is accepted.

**Closure is the agent's responsibility.** The driver never calls
`bd close` on a bead it dispatched. The `bd-closed` column is an
*observable* — the agent invokes `bd close <id>` itself per the run-phase
prompt contract — not a driver action. A driver that auto-closes on
`exit_code == 0` collapses every marker into `done` and silently masks
`LOOM_BLOCKED` / `LOOM_CLARIFY` self-reports, which is why marker
parsing (not exit_code) must be the primary outcome signal for every
phase that spawns an agent.

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

A flag from any review concern produces `recovery` with cause
`review-flag`; the flag detail names the specific concern (see the
per-diff rubric table in [loom-gate.md](loom-gate.md) for the
full set of flag causes). Invariant-clash flags raise `loom:clarify`
instead of entering the recovery loop.

**Self-reports skip recovery.** `LOOM_BLOCKED` and `LOOM_CLARIFY` are agent
self-reports — re-running the same prompt won't recover, so the gate exits
straight to `[blocked]` / `[clarify]` for human resolution.

**Driver-detected causes flow through recovery.** Swallowed marker,
incomplete signaling, zero-progress, verify-fail, and review-flag all
enter the recovery loop. Each recovery iteration either retries the
bead in place with prior failure context, or — when the failure shape
calls for a discrete follow-up unit of work — spawns a **fix-up bead**.

**Fix-up beads bond to the originating molecule.** Every fix-up bead
created during recovery is bonded to the failing bead's molecule via
`bd mol bond <molecule-id> <fix-up-bead-id>` **before dispatch** (i.e.,
before the bead becomes eligible for `loom run` to pick up). The bond
is mandatory and atomic with creation — a fix-up bead that is not
bonded to a molecule by the time it leaves the verdict gate is a bug.

Bonding is load-bearing in two places:

1. The **push gate** refuses to push while any bead in the molecule
   carries `loom:blocked` or `loom:clarify`. Orphan fix-up beads are
   invisible to that check, so a molecule could push with unresolved
   work attached to a shadow bead the gate never saw.
2. **Auto-iteration** (Push gate, Functional #9) walks `bd mol
   progress <id>` to decide whether the molecule is clean. Orphan
   fix-up beads are absent from that walk; the molecule looks done
   even when its remediation work is pending.

The originating molecule is resolved by reading the failing bead's
existing molecule bond — `bd show <id> --json` returns the molecule
ID. If the failing bead is itself unbonded (which is itself a bug
upstream), the verdict gate refuses to spawn a fix-up bead and
escalates to `loom:blocked` with cause `unbonded-origin` so the
inconsistency surfaces immediately rather than propagating.

**Recovery context (`previous_failure`).** On `retry → [running]`, the next
session's prompt is rendered with `previous_failure` populated as a
structured cause + per-cause detail (truncated to 4000 chars):

| Cause | Detail content |
|-------|----------------|
| `swallowed-marker` | "Last phase ended without a `LOOM_*` exit marker." (no further detail) |
| `incomplete-signaling` | "Marker `LOOM_COMPLETE` emitted but bead `<id>` was not bd-closed." |
| `zero-progress` | "Marker `LOOM_COMPLETE` emitted with empty diff. Use `LOOM_NOOP` if no work was needed." |
| `verify-fail` | One block per failing `[verify]` script: path, exit code, last ~40 lines of stderr. All failing scripts are included; the 4000-char budget is split across them with later failures truncated first. If `review` also flagged, its reasoning is appended under `Review notes:` (separate budget, ~1000 chars). |
| `review-flag` | The review LLM's verbatim flag reasoning (typically 1–3 sentences), including which concern triggered the flag — see [loom-gate.md](loom-gate.md) for the full set of flag causes (`spec-coherence-fail`, `orphan-integration`, `verifier-bypass`, `fabricated-result`, `weak-assertion`, `coincidental-pass`, `mock-discipline`, `verifier-too-narrow`, `concurrency-untested`, `scope-creep`, `scope-shortfall`, `judge-flag`). |

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

**Marker definitions.** The agent ends every phase by emitting exactly
one marker on its own line, as the final output of the session:

- `LOOM_COMPLETE` — the work succeeded. The agent has implemented the
  bead's criteria and `bd close`d the bead. The diff is non-empty
  (real changes); see `LOOM_NOOP` below for the zero-diff variant.
- `LOOM_NOOP` — the work was already done in tree; the phase
  intentionally produced an empty diff. Without `LOOM_NOOP`, an empty
  diff is treated as `zero-progress` (a recovery cause). The agent
  emits `LOOM_NOOP` to distinguish "no work needed" from "work
  attempted but produced no diff."
- `LOOM_BLOCKED` — the agent cannot proceed and is self-reporting.
  Write the reason on prior lines before the marker; the gate
  applies `loom:blocked` to *this bead* and exits the verdict
  evaluation without entering recovery. Other beads in the molecule
  continue running; the labelled bead waits for human resolution via
  `loom msg`.
- `LOOM_CLARIFY` — the agent has a specific question with structured
  options for the human (per the [Options Format
  Contract](loom-gate.md#options-format-contract)). Write the
  question / option block on prior lines before the marker; the gate
  applies `loom:clarify` to *this bead* and exits the verdict
  evaluation without entering recovery. Other beads in the molecule
  continue running; the labelled bead waits for `loom msg` resolution.

The gate distinguishes markers by parsing the final line; `exit_code`
alone is insufficient because backend errors, swallowed-marker turns,
and successful self-reports all exit 0.

**Infra failures bypass the gate.** Pre-flight failures (image load, container
start) exit immediately as `blocked` with cause `infra-preflight` — there is
no agent output to evaluate. Mid-session failures (agent process exit
non-zero, container OOM, IO errors) get one free retry per `loom run`,
tracked in driver memory; a second mid-session failure exits as `blocked`
with cause `infra-repeated`. This counter is separate from
`[loop] max_iterations` and does not persist across `loom run` invocations.

### Msg Modes

`loom msg` is the human resolution channel for outstanding `loom:blocked`
and `loom:clarify` beads. Clarify beads carry their options in the
*Options Format Contract* defined in
[loom-gate.md](loom-gate.md#options-format-contract); `loom msg`
consumes that format for list / view / fast-reply / dismiss. The
flag table below documents `loom msg`'s own surface.

**Five modes plus a filter:**

| Mode | Invocation | Where it runs |
|------|-----------|---------------|
| List (default) | `loom msg` | host, no container |
| View | `loom msg -n <N>` / `loom msg -b <id>` | host, no container |
| Fast-reply (option) | `loom msg -n <N> -o <int>` | host, no container |
| Fast-reply (verbatim) | `loom msg -n <N> -r <text>` | host, no container |
| Dismiss | `loom msg -n <N> -d` | host, no container |
| Chat | `loom msg -c` | container, Claude (`msg.md` template) |
| Filter | `-s <label>` (combines with any mode) | scope to `spec:<label>` |

**Flag table.** Both short and long forms are accepted; the long form is
what `loom msg --help` documents.

| Short | Long | Argument | Purpose |
|-------|------|----------|---------|
| `-c` | `--chat` | — | Launch interactive Drafter session in a container |
| `-s` | `--spec` | `<label>` | Filter to clarifies labeled `spec:<label>` |
| `-n` | `--number` | `<int>` | Address a clarify by 1-based list index |
| `-b` | `--bead` | `<bead-id>` | Address a clarify by bead ID |
| `-o` | `--option` | `<int>` | Fast-reply with the bead's `### Option <int>` body; **validated** — errors `option <int> not found in bead <id>` if no matching subsection exists |
| `-r` | `--reply` | `<text>` | Fast-reply with verbatim free-form text; works on any bead regardless of whether it has an Options section |
| `-d` | `--dismiss` | — | Clear the label with a work-around note |

**Mutually exclusive flags.** `-o` and `-r` cannot both be supplied —
passing both errors before any side effects. `-d` cannot combine with
`-o` or `-r`. `-n` and `-b` cannot both be supplied (they're alternative
addressing schemes for the same target). `-c` is mutually exclusive with
all other action flags except `-s`.

**Cross-spec by default.** Bare `loom msg` lists every outstanding
`loom:blocked` and `loom:clarify` bead across all specs, regardless of
the `current_spec` meta value. `-s <label>` is the only narrowing path.
The `current_spec` is not consulted for any msg mode.

**Chat session shape.** `loom msg -c` (optionally with `-s <label>`)
launches the base profile via `wrapix spawn`, runs Claude with the
`msg.md` template, and walks the user through outstanding clarifies
interactively. The session writes resolution notes via
`bd update --notes` and clears the label via `bd update
--remove-label=loom:clarify` (or `loom:blocked`) per resolved bead.
Mid-walk exit is a clean `LOOM_COMPLETE`; unresolved clarifies remain
visible in the next `loom msg` session. The chat session emits
`LOOM_COMPLETE` only — `LOOM_BLOCKED` and `LOOM_CLARIFY` are not valid
exit signals for `msg` (the session itself is the resolution channel,
not a producer of new clarifies).

### Crate Layout

```
loom/
  Cargo.toml                    # workspace root
  crates/
    loom/                       # CLI binary — clap arg parsing, entry point,
      src/                      #   match on AgentKind, wires concrete
        main.rs                 #   backends + workflow + templates
    loom-events/                # PUBLIC CONTRACT: AgentEvent enum + ID
      src/                      #   newtypes (BeadId, MoleculeId, ToolCallId,
        lib.rs                  #   SpecLabel, ProfileName, SessionId,
                                #   RequestId). Three deps: serde, serde_json,
                                #   thiserror. The only crate frontend
                                #   bridges, SSE servers, and external log
                                #   tools depend on. No timestamps crate, no
                                #   ulid, no uuid — `ts_ms: i64`, `seq: u64`.
    loom-driver/                # host-side runtime: AgentBackend trait,
      src/                      #   AgentSession (typestate), LineParse,
        lib.rs                  #   JsonlReader, StateDb (rusqlite), Config,
                                #   BdClient, Clock, profile manifest, lock
                                #   files, scratch dir, git ops (gix +
                                #   shell-out). Re-exports identifier
                                #   newtypes from loom-events for ergonomics.
    loom-render/                # Renderer trait + Pretty/Plain/Json/Raw
      src/                      #   impls. Owns the in-place running
        lib.rs                  #   indicator, OSC 8 hyperlinks, imara-diff
                                #   for Edit/Write tool bodies, per-tool
                                #   summary cells, subagent (Task) nesting,
                                #   driver-event dispatch, color discipline.
                                #   Also owns the LogSink (tee-style writer
                                #   driving disk JSONL + renderer in
                                #   lockstep).
    loom-agent/                 # AgentBackend trait implementations
      src/                      #   (pi, claude). Adapters flatten backend
        lib.rs                  #   wire schemas into loom-events variants;
                                #   synthesize parent_tool_call_id by
                                #   tracking the active Task call stack.
    loom-workflow/              # workflow engine: plan, todo, run, check,
      src/                      #   msg, spec — owns the orchestration loop,
        lib.rs                  #   bead lifecycle, retry logic, push gate,
                                #   verdict gate, driver-event emission.
    loom-templates/             # askama templates + typed context structs.
      src/                      #   layout, partials inventory, per-phase
        lib.rs                  #   pinning policy, agent-output markers,
      templates/                #   and snapshot-test contract are owned by
        ...                     #   loom-templates.md (sibling spec).
```

### Dependency Graph

```
loom (CLI binary)
  ├── loom-events
  ├── loom-driver
  ├── loom-render
  ├── loom-agent
  ├── loom-workflow
  └── loom-templates

loom-events                       # leaf — no internal deps
  └── (serde, serde_json, thiserror)

loom-driver
  └── loom-events                 # AgentEvent, ID newtypes

loom-render
  └── loom-events                 # consumes AgentEvent, owns LogSink

loom-agent
  ├── loom-events                 # produces AgentEvent
  └── loom-driver                 # AgentBackend trait, AgentSession

loom-workflow
  ├── loom-events                 # emits driver_event variants
  ├── loom-driver                 # state, config, bd, locks, git
  ├── loom-render                 # LogSink, Renderer
  └── loom-templates

loom-templates
  └── loom-driver                 # typed context structs
```

`loom-events` is the leaf — frontends and SSE bridges pull only this. The
split is enforced at the dep graph: `loom-events` has no `loom-driver` /
`loom-render` import, so a contract change shows up as a `loom-events`
crate version bump rather than an accidental coupling.

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
imara-diff = "0.1"
crossterm = "0.28"
owo-colors = { version = "4", features = ["supports-colors"] }
colored_json = "5"
```

Eighteen runtime dependencies. New entries scoped to `loom-render`:
`imara-diff` (faster successor to `similar` for `Edit`/`Write` body
diffs), `crossterm` (TTY detection, terminal width, `\r` + clear-to-EOL
for the in-place running indicator, OSC 8 capability detection),
`owo-colors` with the `supports-colors` feature (color discipline +
`NO_COLOR` honoring), `colored_json` (the `Json` renderer mode
pretty-prints with color when TTY, plain when piped). No JSONL-specific
crate — `serde_json` + `BufReader` line splitting is sufficient. No
`async-trait` — `async fn` in traits is stable. `gix` covers read-only
git operations; worktree mutation and merge shell out to the system
`git` CLI — see *Worktree Parallelism* for the split. **Not in
`loom-events`:** none of the four new deps. The contract crate stays at
three deps.

In addition to the eighteen runtime crates, the workspace pins a small
set of test/dev-only crates that don't appear in the runtime dependency
graph but are needed by the build, lint, or test passes. These ride in
`[workspace.dependencies]` so member crates use `workspace = true` for
them too:

- `insta` — snapshot testing (template render parity, CLI help text).
- `proptest` — property tests for parsers and identifier round-trips.
- `tempfile` — scratch directories in integration tests.
- `tokio-test` — async test scaffolding.
- `walkdir` — used by the workspace-internal annotation-integrity test
  binary (`loom/crates/loom/tests/annotations.rs`).
- `pulldown-cmark` — parses spec markdown (`## Companions`, `## Success
  Criteria` annotations) inside `loom-driver` / `loom-templates`.
- `syn` — parses Rust sources for the bidirectional annotation gate
  (`loom/crates/loom/tests/annotations.rs`) which walks every
  `[verify]` / `[judge]` annotation against an existing test function.
- `nix` (the BSD-style POSIX wrapper, not the package manager) — `signal`
  feature, used for SIGINT/SIGTERM handling on cancellation.

Adding a new third-party crate that isn't in either list is a spec
change.

### Workspace Lints

Per [`docs/style-rules.md`](../docs/style-rules.md) RS-3, the lint surface
lives entirely at the workspace root. Two files own it.

**`loom/Cargo.toml`** — `[workspace.lints.rust]` + `[workspace.lints.clippy]`:

```toml
[workspace.lints.rust]
unsafe_code                  = "forbid"
elided_lifetimes_in_paths    = "warn"
explicit_outlives_requirements = "warn"
future_incompatible          = "warn"
nonstandard_style            = "warn"
unused                       = "warn"
missing_copy_implementations = "warn"

[workspace.lints.clippy]
# broad coverage from groups at priority -1 so individual overrides win
all      = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
nursery  = { level = "warn", priority = -1 }

# restriction lints opted in individually — enforce RS-9 and RS-11
unwrap_used   = "warn"
expect_used   = "warn"
panic         = "warn"
todo          = "warn"
unimplemented = "warn"
unreachable   = "warn"
dbg_macro     = "warn"
print_stdout  = "warn"
print_stderr  = "warn"

# pedantic/nursery noise the project doesn't pay for
use_self                    = "allow"
must_use_candidate          = "allow"
uninlined_format_args       = "allow"
significant_drop_tightening = "allow"
too_many_lines              = "allow"
```

Every crate carries `[lints] workspace = true`. No crate-root
`#![warn(...)]` / `#![deny(...)]`. CI runs `cargo clippy -- -D warnings`
so any warning fails the build.

**`loom/clippy.toml`** — test exemptions clippy supports natively:

```toml
allow-expect-in-tests = true
allow-panic-in-tests  = true
allow-unwrap-in-tests = true
allow-print-in-tests  = true
allow-dbg-in-tests    = true
```

Restriction lints without an `allow-*-in-tests` flag (`clippy::todo`,
`clippy::unimplemented`, `clippy::unreachable`) stay warned everywhere —
tests have no legitimate use for them.

Per-site `#[expect(...)]` / `#[allow(...)]` overrides in production
code follow [`docs/style-rules.md`](../docs/style-rules.md) RS-3 —
substantive `reason = "..."` required, generic reasons rejected at
review, audit with `rg '#\[(expect|allow)' -trust`.

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

Each identifier in `loom-events::identifier` is hand-written (no shared macro)
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

`derive(From)` and `derive(Into)` are banned (RS-8) to prevent accidental
bypass of the newtype boundary.

### Askama Template System

See [loom-templates.md](loom-templates.md) — engine choice,
per-template typed context structs, partials inventory, per-phase
pinning policy, agent-output markers, and the snapshot-test contract
all live there. `loom-templates` is the crate;
[loom-templates.md](loom-templates.md) is the spec.

### Beads CLI Wrapper

`loom-driver` provides `BdClient`, a typed wrapper around the `bd` CLI:

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
`loom-driver` and migrated on open (embed migrations via `rusqlite`'s
`execute_batch`).

```sql
CREATE TABLE specs (
    label TEXT PRIMARY KEY
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

CREATE TABLE notes (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    spec_label TEXT NOT NULL REFERENCES specs(label) ON DELETE CASCADE,
    kind       TEXT NOT NULL,
    text       TEXT NOT NULL,
    created_at INTEGER NOT NULL  -- unix millis
);
CREATE INDEX idx_notes_spec_kind ON notes(spec_label, kind);

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- meta rows: current_spec, schema_version, todo_cursor:<label>
```

Typed Rust API — no raw SQL outside `loom-driver`. `loom-driver` owns a single
`StateDb` handle that wraps the SQLite connection and exposes typed
operations: open the DB at a path, fetch a spec row by label, fetch the
active molecule for a spec label (zero or one), get/set the `current_spec`
meta key, increment a molecule's iteration counter, manage notes
(`set`/`add`/`clear`/`list` scoped by `(spec_label, kind)`; `rm` by note
id), and run the rebuild described below. Every operation returns
`Result<_, StateError>`; row shapes are `SpecRow` (label), `MoleculeRow`
(id, spec_label, base_commit, iteration_count), and `NoteRow` (id,
spec_label, kind, text, created_at) — the columns of the `specs`,
`molecules`, and `notes` tables. Notes are one row per note, so
`loom note add` is a single `INSERT`, `loom note rm <id>` is a single
`DELETE`, and `ORDER BY id` yields chronological order without a parse
step.

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

Iteration counters reset to 0 on rebuild. **Notes are lost on rebuild** —
they live only in SQLite and have no filesystem source to reconstruct
from. Rebuild is a clean-slate operation; running it discards all
transient hints, and `loom init --rebuild --help` says so explicitly.
Recovering notes after a rebuild means re-running `loom plan` for the
affected spec. (`loom todo`'s routine consumption is *not* a loss path —
notes are rendered into bead bodies before the rows are deleted; see
*Notes lifecycle* below.)

Total cost: a glob + ~5 `bd` CLI calls + N markdown reads (already loaded
for source #1). Runs in under a second.

**Companion declaration in specs.** Specs declare their companion paths in
a single, parseable section so rebuild is lossless:

```markdown
## Companions

- `lib/sandbox/`
- `loom/crates/loom-templates/`
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

**Notes lifecycle.** Notes are *transient hints* attached to a spec —
bug-or-gotcha context, file paths to touch, design trade-offs left to
the implementer's judgement, decisions captured during a review, etc.
They are never canonical: the spec markdown holds the durable design,
the `notes` table holds the in-flight scratch around it. Notes are
discriminated by `kind`, with `implementation` being the kind consumed
by `loom todo` to seed bead bodies. Other kinds (`decision`, `review`,
`interview-context`, …) can be added additively without a schema change.

The agent never writes notes by editing markdown. It calls a CLI:

```
loom note set   <label> [--kind implementation] --json '["note 1", …]'
loom note add   <label> [--kind implementation] --text "single note"
loom note clear <label> [--kind implementation | --all-kinds]
loom note list  [<label>] [--kind implementation | --all-kinds]
loom note rm    <id>
```

`--kind` defaults to `implementation` so the common case stays terse.
`set` is atomic: `DELETE WHERE spec_label=? AND kind=?` plus N `INSERT`s
in a single transaction. The agent thinks in arrays; storage stays
per-row. Each invocation surfaces in the `AgentEvent` stream as a
regular `tool_call`/`tool_result` pair — visible in `loom logs`,
reproducible in replay, same shape as how the agent already calls
`bd update` and `bd close`.

Lifecycle for `kind = implementation`:

| Event | Effect on `notes` rows where `kind = 'implementation'` |
|-------|--------------------------------------------------------|
| `loom plan -n <label>` | Interview ends by calling `loom note set <label> --json '[…]'`. |
| `loom plan -u <label>` | Interview reads existing notes via `loom note list <label>`, then writes a **merged** array back via `loom note set` — agent's judgement, keeping what still applies, dropping what new decisions invalidate, adding what's fresh. Not a blind append or replace. |
| `loom todo` (productive completion: `(LOOM_COMPLETE or LOOM_NOOP)` AND `exit_code == 0`) | Renders the notes into each new bead body, then atomically deletes them and advances `meta.todo_cursor:<label>` in a single SQLite transaction. |
| `loom todo` (any other terminal state) | Cursor and notes both untouched; next invocation reprocesses the same commit range with the same notes. |
| `loom init --rebuild` | All notes drop with the table — no filesystem source to reconstruct from. |
| Spec file deleted from `specs/` | The `specs` row is orphaned but stays; cleanup is deferred to the next `--rebuild`. |

The `ON DELETE CASCADE` on `notes.spec_label` is dormant — no routine
command `DELETE`s from `specs`, and rebuild drops the table outright.
The clause exists only to keep the FK honest if a future code path ever
takes the explicit-delete route.

**Todo cursor advancement.** `meta.todo_cursor:<label>` records the
last commit at which `loom todo` ran for a spec. The commit-scan tier
uses the cursor to scan only commits since the last run. Advancing it
prematurely silently demotes future invocations to no-ops on real work
the agent never saw, so the cursor is **only** advanced when the
session demonstrates productive completion:

- The agent emitted a `LOOM_COMPLETE` or `LOOM_NOOP` marker on its
  final turn (the completion shapes the verdict gate recognises).
- The session's `exit_code == 0`.

A zero exit alone is not enough — backend errors (529 overload,
network drop, watchdog timeout) and swallowed-marker turns also exit
zero, and treating those as success would skip the spec's commit
range on the next `loom todo` run. On any other terminal state
(`LOOM_BLOCKED`, `LOOM_CLARIFY`, missing marker, nonzero exit) the
cursor stays put so the next invocation reprocesses the same range.

The cursor advance and the implementation-notes delete are two writes
against the same productive-completion gate, so they **must be a single
SQLite transaction**. A delete without a cursor advance (or vice versa)
is a bug. The API exposing the gate
(`StateDb::consume_notes_and_advance_cursor(&label, new_cursor)`) does
both writes atomically inside one `BEGIN`/`COMMIT`, so calling code
cannot perform one without the other.

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

## Affected Files

### New

| File | Role |
|------|------|
| `loom/Cargo.toml` | Workspace root |
| `loom/crates/loom/` | CLI binary |
| `loom/crates/loom-events/` | PUBLIC CONTRACT crate: `AgentEvent` enum, ID newtypes, `schema_version`. Three deps (serde, serde_json, thiserror). Frontends, SSE bridges, and external log tools depend only on this. |
| `loom/crates/loom-driver/` | Host-side runtime: `AgentBackend` trait, `AgentSession`, `LineParse`, `JsonlReader`, `StateDb`, `Config`, `BdClient`, `Clock`, profile manifest, lock files, scratch dir, git ops. |
| `loom/crates/loom-render/` | `Renderer` trait + `Pretty`/`Plain`/`Json`/`Raw` impls; `LogSink` (tee-style writer driving disk JSONL + renderer in lockstep); per-tool summary cells, `imara-diff` for `Edit`/`Write`, OSC 8 hyperlinks, in-place running indicator, subagent (Task) nesting, color discipline. |
| `loom/crates/loom-agent/` | AgentBackend implementations (see [loom-agent.md](loom-agent.md)). Adapters flatten backend wire schemas into `loom-events` variants. |
| `loom/crates/loom-workflow/` | Workflow engine (plan, todo, run, check, msg, spec) — emits `driver_event` variants for verdict gate, retry dispatch, push gate. |
| `loom/crates/loom-templates/` | Askama templates crate. Contents owned by [loom-templates.md](loom-templates.md) |
| `loom/clippy.toml` | Workspace-root clippy config — test exemptions per RS-3 (`allow-expect-in-tests`, `allow-panic-in-tests`, etc.) |
| `.wrapix/loom/config.toml` | Loom config (TOML) |
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

### Removed

| File | Reason |
|------|--------|
| `loom/crates/loom-driver/src/state/implementation_notes.rs` | Markdown `## Implementation Notes` parser (157-line `parse_implementation_notes()` + tests). Notes now write directly to the `notes` table via the `loom note` CLI; nothing parses markdown for notes anymore. |
| `loom/crates/loom-templates/templates/partial/implementation_notes_spec.md` | Partial that instructed the agent to write a `## Implementation Notes` section into the spec markdown. The markdown→parse path is gone. |
| `loom/crates/loom-templates/templates/partial/implementation_notes_state.md` | Sibling partial that rendered the parsed `implementation_notes` markdown blob into bead bodies during `loom todo`. With notes living in the `notes` table and rendered via the typed lifecycle, the partial has no caller. |

### Unchanged

| File | Reason |
|------|--------|
| `lib/city/` | Gas City unchanged |

## Stub-to-real promotion contract

Many criteria below carry `[verify]` annotations whose target functions
in `tests/loom-test.sh` are currently **`exit 77` stubs** —
placeholders that satisfy the bidirectional
annotation-integrity gate (`loom/crates/loom/tests/annotations.rs`)
without yet exercising any code. The stubs share a `_pending_stub`
helper that prints a "pending implementation" line on stderr and exits
77 (POSIX skip).

The promotion contract has two halves:

- **Implementation side.** A PR that lands the production code for a
  criterion **must** replace the matching stub with a real dispatcher
  into the corresponding Rust test (i.e., drop the `_pending_stub` call
  in the same PR). A criterion's checkbox cannot flip from `[ ]` to
  `[x]` while its bash function still routes through `_pending_stub`.
- **Gate side.** The loom gate (see [loom-gate.md](loom-gate.md))
  enforces this on every PR, via the per-diff rubric's *Verifier
  honesty* check: for every criterion the PR claims to implement, the
  gate verifies that
    1. the bash function no longer calls `_pending_stub`, AND
    2. the dispatched Rust test exists and exercises the live path —
       not a `todo!()` body, not a tautological assertion. A stub
       edited to `:` (no-op) and a Rust test asserting `true` both
       fail.

Stubs are tracked under one comment block in `tests/loom-test.sh`
("Pending-implementation stubs"); the block shrinks PR by PR as
implementation lands. When the block is empty, every criterion has a
real verifier and the promotion contract becomes a tautology — that is
the v1-complete signal for this spec.

## Success Criteria

### Crate structure

- [x] Workspace builds with `cargo build` from `loom/` root
  [verify](tests/loom-test.sh::test_workspace_builds)
- [x] All seven crates present: loom, loom-events, loom-driver, loom-render, loom-agent, loom-workflow, loom-templates
  [verify](tests/loom-test.sh::test_crate_structure)
- [x] Workspace uses edition 2024 and resolver "3"
  [verify](tests/loom-test.sh::test_workspace_edition)
- [x] All dependencies pinned under `[workspace.dependencies]`
  [verify](tests/loom-test.sh::test_workspace_deps_pinned)
- [x] All crates declare `[lints] workspace = true`
  [verify](tests/loom-test.sh::test_workspace_lints)
- [x] No `types.rs` or `error.rs` files at crate roots
  [verify](tests/loom-test.sh::test_nested_module_structure)
- [x] Domain identifiers use newtypes (BeadId, SpecLabel, MoleculeId, etc.)
  [verify](tests/loom-test.sh::test_newtypes_for_identifiers)
- [x] No `unwrap()`, `todo!()`, `panic!()`, `unimplemented!()` in non-test code
  [verify](tests/loom-test.sh::test_no_panics_in_production)
- [x] No `#[allow(dead_code)]` in non-test code
  [verify](tests/loom-test.sh::test_no_allow_dead_code)
- [x] No `derive(From)` or `derive(Into)` on newtype structs
  [verify](tests/loom-test.sh::test_no_derive_from_on_newtypes)

### Templates

Owned by [loom-templates.md](loom-templates.md); see that spec's Success
Criteria.

### Process architecture

- [x] Loom never invokes `podman run` directly (grep `loom/crates/` for
      `podman` finds only documentation references)
  [verify](tests/loom-test.sh::test_loom_does_not_invoke_podman)
- [x] `wrapix spawn --spawn-config <file> --stdio` accepts a JSON config,
      reuses container construction from existing `wrapix run`, omits TTY
  [verify](tests/loom-test.sh::test_wrapix_spawn_subcommand)
- [x] `SpawnConfig` JSON shape is stable: serialization round-trip preserves
      all fields and key names, including the `image_ref` and `image_source`
      fields
  [verify](tests/loom-test.sh::test_spawn_config_json_stability)
- [x] `wrapix spawn` runs `podman load` from `image_source` (a Nix store
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

**Renderer modes**

- [x] Four renderer modes implemented: `Pretty`, `Plain`, `Json`, `Raw`
  [verify](tests/loom-test.sh::test_renderer_modes_present)
- [x] `Pretty` is selected when stdout is a TTY and no `--plain`/`--json`/`--raw` flag is set
  [verify](tests/loom-test.sh::test_pretty_selected_on_tty)
- [x] `Plain` is auto-selected on non-TTY stdout (pipe/redirect), `NO_COLOR=1`, or `--plain`
  [verify](tests/loom-test.sh::test_plain_selected_on_non_tty)
- [x] `Json` mode emits one pretty-printed JSON object per line; colorized when TTY, plain when piped
  [verify](tests/loom-test.sh::test_json_mode_pretty_prints)
- [x] `Raw` mode passes through the original JSONL bytes unparsed
  [verify](tests/loom-test.sh::test_raw_mode_passthrough)

**Per-tool rendering**

- [x] Each builtin (`Read`, `Edit`, `Write`, `Grep`, `Glob`, `Bash`, `WebFetch`, `WebSearch`, `Task`) renders its tailored summary cell
  [verify](tests/loom-test.sh::test_per_tool_summary_cells)
- [x] Unknown tools fall through to a generic `<name>  <truncated args>` row
  [verify](tests/loom-test.sh::test_unknown_tool_fallback)
- [x] Tool body is capped at 10 lines or 2 KB (whichever first); cap line names recovery `[N more lines — loom logs -b <id> --tool <id>]`
  [verify](tests/loom-test.sh::test_tool_body_truncation_policy)
- [x] `Edit` and `Write` render unified diffs via `imara-diff`; `+<add> -<del>` counts on the summary cell
  [verify](tests/loom-test.sh::test_edit_write_imara_diff)
- [x] Subagent (`Task`) tool nests inner events under the parent at deeper indent via `parent_tool_call_id`
  [verify](tests/loom-test.sh::test_task_subagent_nesting)
- [x] `tool_call` and `tool_result` collapse into one rendered block; duration computed from `ts_ms` delta
  [verify](tests/loom-test.sh::test_tool_call_result_pairing)

**Driver events**

- [x] `driver_event` variants emit with `source: "driver"` discriminator and render with `→` glyph
  [verify](tests/loom-test.sh::test_driver_events_rendered)
- [x] Verdict gate, retry dispatch, push gate walk/refuse/clean, container spawn/oom all emit `driver_event`
  [verify](tests/loom-test.sh::test_driver_event_kinds_present)
- [x] Unknown `driver_kind` values render as generic `→ <kind>: <summary>` (additive without schema bump)
  [verify](tests/loom-test.sh::test_unknown_driver_kind_renders)

**Live UX**

- [x] In-place running indicator updates duration via `\r` + clear-to-EOL while a tool is in flight
  [verify](tests/loom-test.sh::test_in_place_running_indicator)
- [x] In-place running indicator is auto-disabled in non-TTY modes and with `--parallel N > 1`
  [verify](tests/loom-test.sh::test_in_place_indicator_disabled_when_inappropriate)
- [x] `-v` / `--verbose` disables tool-body truncation, streams `text_delta`/`thinking_delta` live, and shows `thinking` blocks (`◆`)
  [verify](tests/loom-test.sh::test_verbose_full_output)
- [x] Cancellation (Ctrl-C / SIGINT) collapses the in-place indicator and emits a `⚠ interrupted` closing block with partial-diff size
  [verify](tests/loom-test.sh::test_cancellation_clean_close)
- [x] OSC 8 hyperlinks emitted for paths/URLs when terminal supports it (iTerm2, Kitty, WezTerm, recent VS Code, Alacritty, GNOME Terminal); auto-degrades silently on unsupported terminals
  [verify](tests/loom-test.sh::test_osc8_hyperlinks)
- [x] Path normalization: absolute `/workspace/...` paths render repo-relative in tool summary cells
  [verify](tests/loom-test.sh::test_path_normalization_display)

**Replay**

- [x] `loom logs` reuses the same `Renderer` trait + impls as `loom run` (no second formatter)
  [verify](tests/loom-test.sh::test_logs_reuses_renderer)
- [x] Live-vs-replay distinction: `Pretty` renderer takes a `live: bool` parameter; replay suppresses the in-place running indicator and computes durations from `ts_ms` deltas
  [verify](tests/loom-test.sh::test_live_vs_replay_distinction)
- [x] `AgentEvent` derives `Deserialize` so `loom logs` reads its own JSONL files back through the same enum it writes
  [verify](tests/loom-test.sh::test_agent_event_deserialize_round_trip)

**Event schema**

- [x] Every event carries common envelope fields: `kind`, `bead_id`, `molecule_id`, `iteration`, `source`, `ts_ms` (i64 unix millis), `seq` (u64 monotonic per-bead-spawn)
  [verify](tests/loom-test.sh::test_common_envelope_fields)
- [x] `agent_start` carries `schema_version: u32` (currently `1`), `title`, `profile`, `spec_label`, `started_at_ms`
  [verify](tests/loom-test.sh::test_agent_start_fields)
- [x] `seq` is monotonic per bead spawn, starting at `0`
  [verify](tests/loom-test.sh::test_seq_monotonic)
- [x] Variant set is flat (no nested `message_update { delta: ... }`) — top-level `text_delta` / `thinking_delta` / `toolcall_delta` are siblings of `tool_call` / `tool_result`
  [verify](tests/loom-test.sh::test_flat_variant_shape)
- [x] `loom-events` crate has exactly three deps: `serde`, `serde_json`, `thiserror` (no `chrono`, no `ulid`, no `uuid`)
  [verify](tests/loom-test.sh::test_loom_events_minimal_deps)
- [x] Unknown event variants are accepted gracefully (deserialized as a fallback or skipped, never error)
  [verify](tests/loom-test.sh::test_unknown_variants_tolerated)

**Disk log**

- [x] Full raw JSONL event stream is written to
      `.wrapix/loom/logs/<spec-label>/<bead-id>-<timestamp>.jsonl` for every
      bead spawn, regardless of terminal verbosity
  [verify](tests/loom-test.sh::test_run_writes_per_bead_jsonl_log)
- [x] Per-event flush: every `LogSink::emit` call calls `flush()` so `tail -f` and SSE-via-file-watcher consumers see events at emit time
  [verify](tests/loom-test.sh::test_log_sink_per_event_flush)
- [x] Log path is logged at `info!` when the spawn starts
  [verify](tests/loom-test.sh::test_run_logs_log_path)
- [x] With `--parallel N > 1`, each bead writes to its own file (no
      interleaving in a single log)
  [verify](tests/loom-test.sh::test_parallel_logs_are_per_bead)
- [x] Terminal renderer and log writer consume the same `AgentEvent` stream
      (single tee-style sink, not two parallel pipelines)
  [verify](tests/loom-test.sh::test_run_single_event_channel)
- [x] On `loom run` startup, log files older than `[logs] retention_days`
      (default 14) are deleted; recent logs are preserved
  [verify](tests/loom-test.sh::test_log_retention_sweep)
- [x] `[logs] retention_days = 0` disables sweeping (no files deleted)
  [verify](tests/loom-test.sh::test_log_retention_disabled)
- [x] Sweep failures (permission denied, in-use file) do not abort the run
  [verify](tests/loom-test.sh::test_log_retention_failure_tolerance)

**Crate boundary**

- [x] `loom-events` is a leaf crate — no internal deps on `loom-driver` / `loom-render` / `loom-workflow` / `loom-templates`
  [verify](tests/loom-test.sh::test_loom_events_is_leaf)
- [x] `loom-render` depends on `loom-events` only (no `loom-driver`)
  [verify](tests/loom-test.sh::test_loom_render_deps)

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
- [x] `loom todo` implements four-tier detection with per-spec cursor fan-out
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
  [verify](tests/loom-test.sh::test_run_execs_review)
- [x] `loom check` implements push gate (push only on clean completion)
  [verify](tests/loom-test.sh::test_review_push_gate)
- [x] `loom check` auto-iterates on fix-up beads (up to max iterations)
  [verify](tests/loom-test.sh::test_review_auto_iterate)
- [x] Bare `loom msg` lists every outstanding `loom:blocked` and
      `loom:clarify` bead across all specs (cross-spec default); the
      `current_spec` meta value is not consulted
  [verify](tests/loom-test.sh::test_msg_list_cross_spec_default)
- [x] `loom msg -s <label>` (alias `--spec`) filters the list to
      clarifies carrying the `spec:<label>` bead label
  [verify](tests/loom-test.sh::test_msg_spec_filter)
- [x] `loom msg -n <N>` / `loom msg -b <id>` (long forms `--number` /
      `--bead`) views a clarify host-side without launching a container
  [verify](tests/loom-test.sh::test_msg_view_modes)
- [x] `loom msg -n <N> -o <int>` (long form `--option`) writes the bead's
      `### Option <int>` body to notes and clears the label; errors
      `option <int> not found in bead <id>` and exits non-zero if the
      subsection is missing
  [verify](tests/loom-test.sh::test_msg_option_validates)
- [x] `loom msg -n <N> -r <text>` (long form `--reply`) writes verbatim
      text to notes and clears the label, regardless of whether the bead
      has an Options section
  [verify](tests/loom-test.sh::test_msg_reply_verbatim)
- [x] `loom msg -n <N> -d` (long form `--dismiss`) clears the label with
      a work-around note, host-side
  [verify](tests/loom-test.sh::test_msg_dismiss)
- [x] `-o` and `-r` are mutually exclusive; `-d` is mutually exclusive
      with both; `-n` and `-b` are mutually exclusive; passing
      conflicting flags errors before any side effects
  [verify](tests/loom-test.sh::test_msg_flag_exclusivity)
- [x] `loom msg -c` (long form `--chat`) launches an interactive
      Drafter session in a container with the base profile, using the
      `msg.md` template; bare `loom msg` stays host-side
  [verify](tests/loom-test.sh::test_msg_chat_launches_container)
- [x] The chat session writes resolution notes via `bd update --notes`
      and clears the label via `bd update --remove-label=loom:clarify`
      (or `loom:blocked`) per resolved bead
  [verify](tests/loom-test.sh::test_msg_chat_writes_notes)
- [x] The chat session ending mid-walk is a clean `LOOM_COMPLETE`;
      unresolved clarifies remain visible in the next session
  [verify](tests/loom-test.sh::test_msg_chat_partial_progress)
- [x] The chat session's only valid exit signal is `LOOM_COMPLETE`
      (no `LOOM_BLOCKED`, no `LOOM_CLARIFY`, no `LOOM_NOOP`)
  [verify](tests/loom-test.sh::test_msg_chat_exit_signals)
- [x] `loom msg -c` with `-s <label>` scopes the chat session to
      clarifies labeled `spec:<label>`; without `-s`, the session sees
      every outstanding clarify regardless of `current_spec`
  [verify](tests/loom-test.sh::test_msg_chat_scope)
- [x] `loom spec` queries spec annotations (verify/judge)
  [verify](tests/loom-test.sh::test_spec_query)
- [x] `loom spec --deps` scans verify/judge test files in the active spec
      and prints required nixpkgs
  [verify](tests/loom-test.sh::test_spec_deps)

### Verdict gate

- [x] After every agent phase, `loom check` evaluates the result against
      the verdict-gate decision table; mechanical signals (marker,
      bd-closed, diff) make no LLM call
  [verify](tests/loom-test.sh::test_verdict_gate_mechanical_signals)
- [x] `loom run` never invokes `bd close` on a bead it dispatched;
      closure is the agent's responsibility and the `bd-closed` column
      is observed post-hoc. Verified by stubbing an agent that emits
      `LOOM_BLOCKED` / `LOOM_CLARIFY` without calling `bd close` and
      asserting the bead remains open after the run finishes.
  [verify](tests/loom-test.sh::test_run_does_not_close_bead)
- [x] `LOOM_BLOCKED` agent marker → bead transitions to `[blocked]`,
      recovery loop is skipped
  [verify](tests/loom-test.sh::test_gate_loom_blocked_marker)
- [x] `LOOM_CLARIFY` agent marker → bead transitions to `[clarify]`,
      recovery loop is skipped
  [verify](tests/loom-test.sh::test_gate_loom_clarify_marker)
- [x] No marker emitted → recovery with cause `swallowed-marker`
  [verify](tests/loom-test.sh::test_gate_swallowed_marker)
- [x] `LOOM_COMPLETE` + bead not bd-closed → recovery with cause
      `incomplete-signaling`
  [verify](tests/loom-test.sh::test_gate_incomplete_signaling)
- [x] `LOOM_COMPLETE` + closed + empty diff → recovery with cause
      `zero-progress`
  [verify](tests/loom-test.sh::test_gate_zero_progress)
- [x] `LOOM_NOOP` + closed + empty diff → review runs (legitimate no-op
      proceeds to semantic review rather than zero-progress)
  [verify](tests/loom-test.sh::test_gate_loom_noop_empty_diff)
- [x] All `[verify]` scripts on the bead's success criteria run; none
      short-circuit each other; per-script pass/fail + stderr is captured
  [verify](tests/loom-test.sh::test_gate_runs_all_verify_scripts)
- [x] One or more `[verify]` failures → recovery with cause
      `verify-fail`; `previous_failure` carries every failure (not just
      the first), with a 4000-char budget split across them
  [verify](tests/loom-test.sh::test_gate_verify_fail_collects_all)
- [x] Review (LLM step) runs regardless of `[verify]` result; on
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
- [x] Review's secondary concerns are scope appropriateness and
      `[judge]` rubric satisfaction
  [verify](tests/loom-test.sh::test_review_inputs_include_judge_rubrics)
- [x] Review flag → recovery with cause `review-flag`; the flag detail
      names which concern triggered (live-path / mock / scope / judge)
  [verify](tests/loom-test.sh::test_gate_review_flag_names_concern)
- [x] Recovery iter < `[loop] max_iterations` (default 3) → spawns
      fix-up bead OR retries the bead with prior failure context
  [verify](tests/loom-test.sh::test_recovery_under_max)
- [x] Every fix-up bead spawned by the verdict gate is bonded to the
      originating bead's molecule via `bd mol bond` before becoming
      eligible for `loom run` dispatch; the bond is atomic with bead
      creation (no transient orphan window)
  [verify](tests/loom-test.sh::test_fixup_bead_bonded_to_molecule)
- [x] If the originating bead is unbonded (no molecule), the verdict
      gate refuses to spawn a fix-up bead and instead applies
      `loom:blocked` with cause `unbonded-origin` to surface the
      upstream inconsistency
  [verify](tests/loom-test.sh::test_fixup_refuses_unbonded_origin)
- [x] `loom check` push gate walks `bd mol progress <id>` and refuses
      to push when any bead in the molecule — including bonded fix-up
      beads — carries `loom:blocked` or `loom:clarify`; an orphan
      fix-up bead would slip past this check, so the bond invariant
      is what makes the gate sound
  [verify](tests/loom-test.sh::test_push_gate_sees_fixup_beads)
- [x] Recovery iter ≥ max_iterations → applies `loom:blocked` with cause
      in `bd update --notes`
  [verify](tests/loom-test.sh::test_recovery_exhaustion_applies_blocked)
- [x] Iteration count is bead-level state and survives `retry →
      [running]` round-trips
  [verify](tests/loom-test.sh::test_iteration_count_persists)
- [x] Pre-flight infra failures (image load, container start) exit
      immediately as `loom:blocked` with cause `infra-preflight`; no retry
  [verify](tests/loom-test.sh::test_infra_preflight_fail_fast)
- [x] Mid-session infra failures (agent process exit non-zero, container
      OOM, IO errors) get one free retry per `loom run`; second mid-
      session failure → `loom:blocked` with cause `infra-repeated`
  [verify](tests/loom-test.sh::test_infra_midsession_one_retry)
- [x] Infra-retry counter is driver-memory only; resets on a fresh
      `loom run` invocation; does not consume `[loop] max_iterations`
  [verify](tests/loom-test.sh::test_infra_retry_counter_separate)
- [x] `loom check` push gate refuses to push while any bead in the
      molecule carries `loom:blocked` or `loom:clarify`
  [verify](tests/loom-test.sh::test_push_gate_refuses_unresolved)

### Auxiliary commands

- [x] `loom init` creates `.wrapix/loom/config.toml` and `.wrapix/loom/state.db`
      with the default schema
  [verify](tests/loom-test.sh::test_init_creates_state)
- [x] `loom init --rebuild` repopulates the state DB from `specs/*.md`
      and active beads
  [verify](tests/loom-test.sh::test_init_rebuild)
- [x] `loom status` prints active spec, current molecule, iteration count
      from the state DB
  [verify](tests/loom-test.sh::test_status_command)
- [x] `loom use <label>` sets `current_spec` in the state DB; round-trips
      with `loom status`
  [verify](tests/loom-test.sh::test_use_command)
- [x] Bare `loom logs` pretty-renders the most recent bead's full log
      via the same `AgentEvent` renderer used by `loom run`, then
      exits at EOF (no implicit follow); `-b <id>` (long form
      `--bead`) selects a specific bead's log
  [verify](tests/loom-test.sh::test_logs_default_renders_and_exits)
- [x] `loom logs -f` (long form `--follow`) tails the selected log,
      blocking on EOF until the file grows or the user interrupts
  [verify](tests/loom-test.sh::test_logs_follow_blocks_on_eof)
- [x] `loom logs --raw` emits raw JSONL bytes from the file, unparsed;
      `loom logs -f --raw` tails raw JSONL (composes with follow)
  [verify](tests/loom-test.sh::test_logs_raw_and_follow_compose)
- [x] `loom logs --path` prints the resolved log file path and exits;
      mutually exclusive with `-f`, `-v`, and `--raw` (passing any of
      those alongside `--path` errors before opening the file)
  [verify](tests/loom-test.sh::test_logs_path_short_circuits)
- [x] `loom logs -v` (long form `--verbose`) streams assistant text
      deltas during render, matching `loom run -v` output
  [verify](tests/loom-test.sh::test_logs_verbose_streams_deltas)
- [x] Bare `loom logs` against an empty `.wrapix/loom/logs/` exits 0
      with a one-line "No bead logs yet" message; `loom logs --path`
      in the same state exits non-zero with a clear error
  [verify](tests/loom-test.sh::test_logs_empty_directory)
- [x] `loom logs` and `loom run` share a single renderer; the
      `AgentEvent` consumer used to format live output is the same
      module used to replay saved logs (no second formatter)
  [verify](tests/loom-test.sh::test_logs_shares_renderer_with_run)
- [x] No `loom sync` / `loom tune` commands exist (compiled templates make
      them unnecessary)
  [verify](tests/loom-test.sh::test_no_sync_or_tune_command)

### State database

- [ ] `StateDb::open` creates tables on first open
  [verify](tests/loom-test.sh::test_state_db_init)
- [x] `StateDb::rebuild` populates from spec files and active beads
  [verify](tests/loom-test.sh::test_state_db_rebuild)
- [x] `StateDb::rebuild` parses each spec's `## Companions` section and
      writes one `companions` row per listed path; specs without the
      section contribute zero rows (not an error)
  [verify](tests/loom-test.sh::test_state_db_rebuild_companions)
- [x] `StateDb::rebuild` resets iteration counters to 0
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
- [x] `loom todo` advances the cursor only when the session emitted a
      `LOOM_COMPLETE` or `LOOM_NOOP` marker **and** `exit_code == 0`;
      any other terminal state (no marker, nonzero exit, `LOOM_BLOCKED`,
      `LOOM_CLARIFY`) leaves the cursor untouched
  [verify](tests/loom-test.sh::test_todo_cursor_advance_requires_marker)
- [x] The implementation-notes delete and the cursor advance share one
      SQLite transaction, both gated on productive completion; a
      non-productive terminal state leaves both intact
  [verify](tests/loom-test.sh::test_todo_delete_notes_atomic_with_cursor)
- [ ] `loom plan -n <label>` inserts a `specs` row and seeds
      implementation notes via `loom note set` from the interview
  [verify](tests/loom-test.sh::test_plan_new_writes_implementation_notes)
- [ ] `loom plan -u <label>` reads the existing implementation notes
      via `loom note list`, and writes back a merged array via
      `loom note set` (interview-driven keep/drop/add — not blind
      append, not blind replace)
  [judge](tests/judges/loom.sh::judge_plan_update_merges_notes)
- [x] `loom todo` reads implementation notes from the anchor's `notes`
      rows and renders each note's text into every new bead body
      created during the run
  [verify](tests/loom-test.sh::test_todo_renders_notes_into_beads)
- [x] `loom note set <label> --kind <k> --json '[…]'` is atomic —
      `DELETE WHERE spec_label=? AND kind=?` plus N `INSERT`s in one
      transaction; partial failure leaves the prior set intact
  [verify](tests/loom-test.sh::test_loom_note_set_atomic)
- [x] `loom note add <label> --kind <k> --text "…"` appends a single
      row to `notes`
  [verify](tests/loom-test.sh::test_loom_note_add)
- [x] `loom note rm <id>` deletes by primary key
  [verify](tests/loom-test.sh::test_loom_note_rm)
- [x] `loom note list [<label>]` returns rows for the spec/kind pair
      (default kind: `implementation`) ordered by `id` ascending
      (chronological); `--all-kinds` widens to every kind and includes
      the `kind` column in output
  [verify](tests/loom-test.sh::test_loom_note_list_chronological)
- [x] `loom note clear <label>` deletes rows for the spec/kind pair
      (default kind: `implementation`); `--all-kinds` wipes every kind
      for the spec in one statement
  [verify](tests/loom-test.sh::test_loom_note_clear)
- [x] `--kind` defaults to `implementation` on every subcommand that
      accepts it, so `loom note add my-spec --text "…"` is the
      common-case shorthand
  [verify](tests/loom-test.sh::test_loom_note_kind_defaults_implementation)
- [x] `loom init --rebuild` drops and recreates the `notes` table —
      no notes survive a rebuild, regardless of `kind`
  [verify](tests/loom-test.sh::test_rebuild_drops_all_notes)
- [x] `notes.spec_label` is declared with `ON DELETE CASCADE`; an
      explicit `DELETE FROM specs WHERE label = ?` removes the notes in
      the same statement. No routine command takes that path today —
      this verifies the FK clause itself
  [verify](tests/loom-test.sh::test_notes_cascade_on_spec_delete)
- [x] Routine commands never DELETE a `specs` row; row removal happens
      only via `loom init --rebuild`
  [verify](tests/loom-test.sh::test_routine_commands_never_delete_spec_row)

### Compaction recovery

- [x] At session start, `.wrapix/loom/scratch/<key>/` contains
      `prompt.txt`, `scratch.md`, `repin.sh` for every phase command
      (plan, todo, run, check, msg)
  [verify](tests/loom-test.sh::test_scratch_dir_created)
- [x] `<key>` is the spec label for plan/todo phases and the bead ID for
      run/check/msg phases
  [verify](tests/loom-test.sh::test_scratch_key_naming)
- [x] Running `repin.sh` emits a valid `SessionStart[compact]` JSON
      envelope containing banner + `prompt.txt` + `scratch.md` contents
  [verify](tests/loom-test.sh::test_repin_envelope)
- [x] `claude-settings.json` registers `repin.sh` under
      `SessionStart[matcher: compact]`
  [verify](tests/loom-test.sh::test_repin_hook_registered)
- [x] On session end (success or failure), the per-key scratch directory
      is removed
  [verify](tests/loom-test.sh::test_scratch_dir_cleanup)
- [x] Two parallel `loom run` workers on different beads use independent
      scratch directories and do not collide
  [verify](tests/loom-test.sh::test_parallel_scratch_isolation)
- [ ] `partial/scratchpad.md` instructs the agent that the scratchpad is
      agent-lifecycle-only and points at durable destinations for
      long-term records
  [judge](tests/judges/loom.sh::test_scratchpad_partial_clarity)

### Beads CLI wrapper

- [x] `bd show` output parsed into typed `Bead` struct
  [verify](tests/loom-test.sh::test_bd_show_parsing)
- [x] `bd list` output parsed with label and status filtering
  [verify](tests/loom-test.sh::test_bd_list_parsing)
- [x] `bd create` returns created bead ID
  [verify](tests/loom-test.sh::test_bd_create_returns_id)
- [x] CLI errors mapped to typed error variants
  [verify](tests/loom-test.sh::test_bd_error_handling)

### Nix integration

- [x] Loom binary builds via `nix build`
  [verify](tests/loom-test.sh::test_nix_build)
- [x] Loom binary is available in the devShell
  [verify](tests/loom-test.sh::test_devshell_includes_loom)
- [x] `cargo clippy` passes with workspace lints
  [verify](tests/loom-test.sh::test_clippy_clean)
- [x] `cargo test` passes for all crates
  [verify](tests/loom-test.sh::test_cargo_test)

## Out of Scope

- **Gas City integration** — Gas City is experimental and token-heavy. Loom
  does not need to integrate with or replace Gas City's agent management.
- **Agent backend implementations** — defined in [loom-agent.md](loom-agent.md).
- **Parallelism beyond worktree-per-bead** — `loom run --parallel N`
  dispatches one git worktree per bead in parallel. New parallelism
  strategies (cross-spec, distributed, scheduler-aware) are future
  work.
- **Hidden specs (`-h` flag)** — scratch / private specs are not a
  first-class concept. The use case — keeping a spec out of git — is
  covered by `.git/info/exclude` on `specs/<label>.md`. Eliminating
  the flag keeps `plan` / `todo` / `run` path-resolution
  single-shaped. Reintroducing it later is a non-breaking additive
  change if the workflow asks for it.
- **Per-project template customization** — loom templates are Askama,
  compiled into the binary. There is no per-project template-fetch /
  template-tune mechanism. Project-specific prompt tweaks happen via
  `pinned_context` / `style_rules` config and per-spec `notes`.
  Project-specific prompt tweaks happen via `pinned_context` and the
  per-spec `notes` mechanism, not by editing template source.
- **Observation daemon** — a polling monitor that spawns short-lived
  agent sessions to observe tmux / browser logs and create beads for
  detected issues. Independent of the workflow phase set; deferred to
  a follow-up spec if and when the use case re-emerges.
- **Session persistence across container restarts** — each container starts a
  fresh agent session.

## Configuration

Loom reads `.wrapix/loom/config.toml` — TOML, parsed natively via the
`toml` crate into a typed `LoomConfig` struct with `#[serde(default)]`
on all fields so the file can be empty or absent (defaults apply).

```toml
# Project overview — pinned in every phase via partial/context_pinning.md
pinned_context = "docs/README.md"

# Rust / project style rules — pinned in run + check via partial/style_rules.md
# (see loom-templates.md for the partial inventory and pinning policy)
style_rules = "docs/style-rules.md"

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

Defaults are chosen so the file can be absent on a fresh install and
loom still works. Concerns that don't appear as config fields (output
display, hook integration, watch behaviour, failure-pattern handling)
are handled in Rust code rather than exposed as user-tunable
parameters.

## Implementation Notes

- `loom msg` fast-reply (`-a` in current impl, spec documents `-o`/`-r`)
  builds the composed answer but never persists it. Fix at
  `loom/crates/loom/src/main.rs:885-892`: add `notes` (the `FastReply`'s
  composed text) to `UpdateOpts` alongside the existing `remove_labels`
  field, so the single `bd update` call carries both writes.
- `loom run` decides `AgentOutcome` purely on `exit_code` at
  `loom/crates/loom-workflow/src/run/production.rs:181-188`, ignoring the
  agent's `LOOM_BLOCKED` / `LOOM_CLARIFY` marker. Add `Blocked { reason }`
  and `Clarify { question }` variants to `AgentOutcome` in
  `loom/crates/loom-workflow/src/run/outcome.rs`, parse the marker (the
  parser already exists — `loom todo` uses it at `todo/production.rs:228`
  and `loom check` at `check/phase_verdict.rs:142`), and route through
  `BeadResult::Blocked` / a new `Clarified` self-report variant so
  `apply_blocked` / `apply_clarify` add the right label and the bead
  stays open. `BeadResult::Clarified` is currently documented as
  "retries exhausted" — keep that meaning, add a separate variant for
  agent self-reports.
- Audit `loom run`'s bead lifecycle for any path that calls `bd close`
  on the dispatched bead; remove it. Closure is the agent's job per the
  run-phase prompt contract; the gate only observes.
- Promote the matching `exit 77` stubs to real end-to-end tests so this
  regression cannot recur silently:
  - `test_msg_option_validates` / `test_msg_reply_verbatim` — after the
    fast-reply call, assert `bd show <id> --json` returns the composed
    note in the `notes` field AND the label is gone.
  - `test_gate_loom_blocked_marker` / `test_gate_loom_clarify_marker` —
    stub agent stdout to emit only the marker (no `bd close`); after the
    run, assert the bead is open and carries the right label.
  - `test_run_does_not_close_bead` — same stub harness, assert the
    driver issued no `bd close` for the dispatched bead.
