# Beads Issue Tracking

Lightweight issue tracker with first-class dependency support, used as the
persistence layer for loom-driven AI agent workflows.

## Problem Statement

AI coding agents need persistent issue tracking that survives across sessions
and context windows, tracks dependencies between tasks, syncs between host and
container environments, and provides a CLI suitable for agent use. Loom drives
this tracker: bead state IS the molecule state, so beads availability and
correctness gate loom's ability to make progress.

## Architecture

Beads is an external CLI (`bd`) backed by a Dolt SQL database. Wrix
provides:

- A **Dolt service** inside the repository-root service container
  (`<repo>-service`, owned by `services.md`) serving `.beads/dolt` on
  identity-hashed endpoints. One service container exists per service
  identity path and is reused across invocations; Loom paths under `.loom/`
  share the outer repository identity so bead clones do not create their own
  service containers.
- A **shellHook** that ensures the service container is running and exports
  `BEADS_DOLT_SERVER_*` env vars so `bd` connects through the Dolt service
  rather than embedding Dolt in-process.
- **Wrix CLI integration** on the devShell PATH: `cli.md` owns the root
  command grammar; `services.md` owns `wrix service ...` service lifecycle and
  Dolt endpoint diagnostics; this spec owns the behavior behind
  `wrix beads push` session-close branch sync.

### Container lifecycle isolation

The service container started by `beads.shellHook` / `services.md` has a
lifecycle independent of the process that triggered shellHook evaluation.
Stopping or restarting the caller (a shell, an editor process, a systemd user
service) does not block on container teardown, nor cause the service
container to be SIGKILLed as a side effect of the caller's stop timeout.

Rationale: shellHook fires from any process that enters the devShell —
direnv, `nix print-dev-env`, an editor's `.envrc`-driven evaluation. When
the caller is a long-running service (e.g. an emacs daemon under
`systemd --user` with `envrc-mode` active), the container otherwise
inherits the caller's cgroup. Stopping the caller then waits the full
`TimeoutStopSec` before SIGKILL because conmon keeps the cgroup populated
— and SIGKILL takes out both the caller and the container.

The mechanism is platform-conditional (e.g. `systemd-run --user --scope` on
systemd-based Linux; Darwin's Apple `container` and podman-via-VM already
satisfy the invariant via separate process trees). The spec states the
invariant; the implementation chooses the mechanism per platform.

## Command Surface

`bd …` commands are upstream beads. `wrix beads push` is the Wrix-provided
session-close wrapper; `cli.md` owns its CLI placement and this spec owns its
sync behavior.

| Command | Purpose |
|---------|---------|
| `bd ready` | Show issues ready to work (no blockers) |
| `bd list --status=<state>` | List issues by status |
| `bd show <id>` | Issue details with dependencies |
| `bd create --title=… --type=task --priority=N` | Create issue (priority 0-4, alias P0-P4) |
| `bd update <id> --status=in_progress` | Claim work |
| `bd update <id> --add-label=<label>` / `--remove-label=<label>` | Manage labels |
| `bd update <id> --notes=…` | Set issue notes |
| `bd close <id>` | Close issue |
| `bd dep add <issue> <depends-on>` | Add dependency |
| `bd dolt pull` / `bd dolt push` | Sync the local Dolt database against the remote |

Issue types: task, bug, feature, epic, question, docs. Priority levels
range from 0 (critical) to 4 (backlog); the `P0`–`P4` form is accepted
as an alias.

## Storage

Beads splits state across the main worktree and a dedicated `beads`
branch worktree:

- **Main worktree `.beads/`** holds the repository config (`config.yaml`)
  and database metadata (`metadata.json`) — the only files git tracks
  under `.beads/`. Everything else is gitignored: the local Dolt database
  (`dolt/`, served by the per-workspace service container), the JSONL backup
  (`backup/`), and runtime state (`bd.sock`, lock/log files, sync state).
- **Beads branch worktree** at `.git/beads-worktrees/beads/.beads/` holds
  `dolt-remote/`, the canonical Dolt remote committed on the `beads`
  branch.

`bd dolt pull` / `bd dolt push` move data between the local `dolt/` and
the remote (on disk in the beads branch worktree).
`bd dolt push` alone does not move the `beads` git branch to GitHub —
that's the role of `wrix beads push` (see *Session-Close Sync*).

The shellHook exports the connection info `bd` uses to reach the Dolt
service inside the repository-root `<repo>-service`: `BEADS_DOLT_SERVER_SOCKET`
on Linux, `BEADS_DOLT_SERVER_HOST` and `BEADS_DOLT_SERVER_PORT` on Darwin.

Sandbox launchers stage only the beads files needed to locate the Dolt-backed
workspace (`config.yaml` and `metadata.json`). They do not stage
`.beads/issues.jsonl`; JSONL is a backup/export artefact, not a live recovery
source for sandboxed clients. A sandbox whose Dolt service endpoint is missing
fails loudly rather than falling back to JSONL import or embedded Dolt.

## Configuration

Key settings in `.beads/config.yaml`:

| Setting | Purpose |
|---------|---------|
| `issue-prefix` | Prefix for issue IDs (e.g., "wx" → "wx-1") |
| `sync-branch` | Git branch for beads data |
| `sync.mode` | Sync mode: `dolt-native` |
| `export.auto` | bd JSONL auto-export toggle; set `false` by `wrix beads push` (see *Auto-export suppression*) |

## Session-Close Sync

`wrix beads push` is the session-close synchronization step: it lands local
operator state (`bd close`, `bd update --status=…`, label changes) into
the on-disk Dolt remote, then pushes the `beads` git branch to GitHub.

### Invocation contexts

`wrix beads push` is safe to invoke unconditionally — it detects its context and
acts accordingly rather than requiring consumers to guard the call. A
consumer's session-close step is therefore an unconditional `wrix beads push`;
no `$LOOM_INSIDE` check and no manual dolt-sync-race workaround belong in
downstream "land the plane" instructions.

- **Loom-managed bead clone (`$LOOM_INSIDE` set).** Full no-op: no git or
  dolt operation runs and `wrix beads push` exits 0 with a one-line notice.
  Inside a loom clone `origin` points at the driver workdir (not GitHub)
  and `.git/beads-worktrees/<branch>` does not exist; the loom driver
  publishes `main` + `beads` after a Clean review verdict, and the worker's
  `bd` writes are already authoritative through the bind-mounted Dolt
  socket. Running `bd dolt push` here would target the wrong remote and add
  a second writer racing the driver, so `wrix beads push` declines entirely.
- **Unresolvable repository (`$LOOM_INSIDE` unset).** When
  `git rev-parse --show-toplevel` does not resolve a workspace root,
  `wrix beads push` fails fast with an actionable stderr message naming the
  unresolved repository, rather than letting an empty `ROOT` flow into a
  git invocation that prints `fatal: not a git repository: (null)`.
- **Normal host session.** The dolt-sync (see *Ordering*) and beads-branch
  sync run as described below, including the existing skip when no
  `<branch>` resolves locally or on `origin`.

This context guard runs first — before the auto-export write and any
`bd dolt` call — so a loom-clone invocation has zero side effects.

### Ordering

`wrix beads push` attempts `bd dolt push` before `bd dolt pull`. When the
push succeeds, the local state reaches the remote verbatim and no merge
is computed. Only when the push fails because the remote has advanced
(a fast-forward rejection from Dolt) does `wrix beads push` fall back to
pull-then-push. Other push failures (network, auth, disk) propagate
as-is rather than triggering the fallback.

Rationale: an interior `pull` ahead of `push` runs Dolt's default
three-way merge over rows the local session has just written. Dolt's
default policy can pick the remote side over the local side without
surfacing a conflict, silently reverting operator state (e.g. a
`bd close` returning to `blocked`) before the push that would have
landed it. Pushing first ensures the local commits become the merge
base for any subsequent remote activity rather than being merged
against pre-write remote state.

### Pull-fallback intent protection

On the pull-fallback path, `wrix beads push` must not silently overwrite
local operator state. Before `bd dolt pull`, it records
`(issue_id, intended_status, intended_labels)` for every row where the
local-ahead-of-remote commits modified `status` or `labels` (sourced
from a Dolt system-table column diff between local HEAD and the remote
tracking branch). After the pull, it re-reads those rows; any row
whose post-pull value diverges from the recorded intent causes
`wrix beads push` to exit non-zero with the affected issue IDs in stderr.
No push is attempted. The operator resolves the conflict by hand.

### Auto-export suppression

`wrix beads push` disables bd's auto-export hook
(`bd config set export.auto false`) on every invocation that proceeds past
the context guard (a `$LOOM_INSIDE` no-op runs nothing at all). The hook is
redundant with the explicit `bd dolt` calls the script already makes,
and its `git add .beads/issues.jsonl` produces noisy warnings in any
repo that gitignores the JSONL (the common case post-Dolt, including
wrix itself). The write is idempotent and persists in
`.beads/config.yaml`, so subsequent bd calls in that repo skip
auto-export until a consumer explicitly re-enables it.

### Beads-worktree resilience

The beads-branch sync recreates the beads worktree when it is absent **or**
present-but-invalid. The recreate guard fires both when the worktree
directory does not exist and when it exists but is no longer a valid git
worktree — e.g. its admin directory `.git/worktrees/<branch>` was pruned or
removed, leaving a dangling gitdir pointer. In the invalid case `wrix beads push`
prunes the stale worktree registration, removes the directory, and re-adds
the worktree relative to the current `$ROOT` (falling back to
`origin/<branch>`, or skipping with a notice when no `<branch>` exists).
Rebuilding relative to the running `$ROOT` keeps the pointers correct from
both host and container contexts — the checkout is bind-mounted, so worktree
admin files store context-dependent absolute paths.

Removing the directory is non-destructive: the canonical bead data has
already been pushed to the on-disk Dolt remote earlier in this run, the
`<branch>` commits live in the shared `.git` object store rather than inside
the worktree, and the worktree's `dolt-remote/` copy is rsynced fresh from
`$REMOTE_DIR` on every run. Recovery only triggers on an already-invalid
worktree, whose contents were inaccessible regardless.

Every git invocation in the beads-branch sync — including the
`git worktree add` that recreates the worktree — uniformly skips prek,
because the `beads` branch legitimately carries no prek config and a
post-checkout hook firing there would abort the sync with
`No prek.toml … found`. The prek-skip is scoped to this section; the
preceding Dolt commit/push phase is unchanged.

### Pre-pull cleanup

Before `git pull --rebase` in the beads worktree, `wrix beads push` commits
any pre-existing dirt (untracked files, modified tracked files) from
prior interrupted runs that would otherwise abort the rebase. The
detection surface is the same one `git rebase` itself consults —
`git status --porcelain` after `git update-index --refresh` — so a
stale stat cache cannot hide a change from the cleanup that the rebase
will then refuse on.

## Loom Integration

Beads is the persistence layer for loom's molecule state. Bead rows hold
issue state, dependency edges, labels, and notes that loom subcommands
read and write:

- `loom plan` writes implementation notes via `loom note set` (backed by
  `bd update --notes`); later sessions (`loom todo`, `loom loop`) read them.
- `loom todo` creates beads from spec changes via `bd create` and links
  dependencies via `bd dep add`.
- `loom loop` claims a bead via `bd update --status=in_progress`, completes
  the work, and closes via `bd close`.
- `loom gate verify` enumerates a bead's acceptance criteria; on
  `loom:blocked` / `loom:clarify` labels the bead waits for `loom msg`
  resolution.
- `loom msg` reads and responds to labelled beads via `bd list
  --label=loom:clarify`, `bd update --remove-label=…`, and writes Options
  Format Contract content into the notes field.

Beads owns the lifecycle primitives; loom owns the policy on top of them.
The Options Format Contract for `loom:clarify` notes is defined by loom
upstream, not by this spec.

## Success Criteria

- `bd dolt pull` and `bd dolt push` succeed inside the wrix sandbox using
  staged beads config plus the shared Dolt service, with no fallback to a
  per-container embedded Dolt or JSONL import
  [system](verify:beads.dolt-sync-in-container)

- Direct `bd dolt pull` / `bd dolt push` inside the sandbox temporarily use
  the container-visible `/workspace` beads-worktree remote when the persisted
  Dolt `origin` points at the host checkout path, and restore the persisted
  remote after the command
  [system](verify:beads.dolt-sync-uses-container-remote)

- `beads.shellHook` launches the workspace service container with a lifecycle independent
  of the caller, so stopping a long-running parent (e.g. a `systemd --user`
  service that triggered shellHook evaluation via envrc) does not block on
  container teardown nor deliver SIGKILL to the service container as a side effect
  of the caller's stop timeout
  [system](verify:beads.shellhook-lifecycle-isolation)

- The same service identity path yields the same `<repo>-service` container name and Dolt endpoint across
  `beads.shellHook` invocations; different checkout identity paths yield
  different names and endpoints
  [system](verify:beads.workspace-naming-determinism)

- `beads.shellHook` fails non-zero with a stderr message when no container
  runtime is available or when Dolt does not become reachable within the
  startup budget — no fallback to embedded Dolt
  [system](verify:beads.shellhook-fail-loud)

- Sandboxed clients receive staged beads config/metadata but not `.beads/issues.jsonl`, so a missing Dolt endpoint fails loudly instead of triggering JSONL auto-import or embedded Dolt recovery
  [system](verify:beads.no-jsonl-staged)

- `wrix beads push` attempts `bd dolt push` before `bd dolt pull`, so a
  session-close run against an up-to-date remote never enters the Dolt
  merge path
  [test](crates/wrix-beads/tests/push_workflow.rs::push_precedes_pull)

- On the pull-fallback path, `wrix beads push` snapshots local `status` and
  `labels` intent before pulling and exits non-zero with the affected
  issue IDs in stderr when the post-pull row state diverges from that
  intent — no push attempted, no silent overwrite
  [test](crates/wrix-beads/tests/push_workflow.rs::pull_fallback_preserves_local_intent)

- `wrix beads push` disables bd's auto-export hook on every invocation that
  proceeds past the context guard (idempotent), leaving
  `export.auto: false` persisted in
  `.beads/config.yaml`, so subsequent bd calls inside and outside
  `wrix beads push` no longer emits the `Warning: auto-export: git add failed`
  message or write `.beads/issues.jsonl`
  [test](crates/wrix-beads/tests/push_workflow.rs::disables_auto_export_idempotently)

- On host invocations, `wrix beads push` repairs a missing or stale Dolt `origin`
  remote to the current checkout's host-path beads worktree remote before
  Dolt sync; sandbox/container invocations temporarily point `origin` at the
  current checkout's beads worktree remote for the sync and restore the prior
  remote before exit, so a `/workspace` path is not left in shared Beads config
  [test](crates/wrix-beads/tests/push_workflow.rs::repairs_or_temporarily_overrides_dolt_origin)

- When `$LOOM_INSIDE` is set, `wrix beads push` performs no git or dolt
  operation and exits 0 with a one-line notice, so a consumer may invoke it
  unconditionally inside a loom-managed bead clone — where `origin` points
  at the driver workdir and `.git/beads-worktrees/<branch>` is absent —
  without error and without a second writer racing the driver
  [test](crates/wrix-beads/tests/push_workflow.rs::loom_inside_is_noop)

- When `$LOOM_INSIDE` is unset and `git rev-parse --show-toplevel` does not
  resolve a workspace root, `wrix beads push` exits non-zero with an actionable
  stderr message naming the unresolved repository — never proceeding with an
  empty `ROOT` into a git invocation that prints
  `fatal: not a git repository: (null)`
  [test](crates/wrix-beads/tests/push_workflow.rs::missing_repo_fails_before_git_sync)

- `wrix beads push`'s pre-pull cleanup commits any pre-existing dirt in the
  beads worktree — untracked files OR modified tracked files left by a
  previously-interrupted run — using the same detection surface
  `git rebase` itself consults, so the subsequent `git pull --rebase`
  never aborts with "You have unstaged changes"
  [test](crates/wrix-beads/tests/push_workflow.rs::pre_pull_cleanup_uses_canonical_dirty_detection)

- When the beads worktree directory exists but is no longer a valid git
  worktree (its `.git/worktrees/<branch>` admin directory was pruned or
  removed, leaving a dangling gitdir), `wrix beads push` prunes, removes, and
  recreates the worktree relative to the current `$ROOT`, then completes the
  beads-branch sync — exiting 0, printing `wrix beads push: synced to GitHub`, and
  advancing `origin/<branch>` with no `fatal: not a git repository: (null)`
  error
  [test](crates/wrix-beads/tests/push_workflow.rs::recovers_orphaned_worktree_relative_to_root)

- Every git invocation in the beads-branch sync, including the
  `git worktree add` that recreates the worktree, skips prek, so a fresh
  worktree recreate completes without the caller setting
  `PREK_ALLOW_NO_CONFIG` and without a `No prek.toml … found` error from the
  config-less `beads` branch
  [test](crates/wrix-beads/tests/push_workflow.rs::git_sync_invocations_skip_prek)

## Requirements

### Functional

1. **Issue CRUD** — Create, read, update, and close issues via `bd`.
2. **Dependencies** — `bd dep add` records blocking relationships; `bd ready`
   excludes blocked issues.
3. **Labels and notes** — `bd update --add-label` / `--remove-label` /
   `--notes` provide the surfaces loom uses for its resolution loop and
   Options Format Contract content.
4. **Sync** — `bd dolt pull` / `bd dolt push` operate over the Dolt remote
   in the `beads` branch worktree.
5. **Per-workspace Dolt service** — `beads.shellHook` reaches Dolt through
   the `<repo>-service` container defined in `services.md`; the Dolt endpoint
   is deterministic from the service identity path (sha256-based) so concurrent
   workspaces do not collide.
6. **Lifecycle isolation** — the service container started by
   `beads.shellHook` has a lifecycle independent of the process that
   triggered its evaluation.
7. **Session-close sync** — `wrix beads push` attempts `bd dolt push` before
   `bd dolt pull`; on the pull-fallback path it snapshots local `status`
   and `labels` intent before pulling and refuses to overwrite divergent
   rows.
8. **Auto-export suppression** — `wrix beads push` disables bd's auto-export
   hook on every invocation past the context guard (idempotent); the
   pre-pull cleanup in the beads worktree uses `git status --porcelain` so
   any dirt the rebase would refuse is committed first.
9. **Dolt origin remote handling** — on host invocations, `wrix beads push`
   repairs a missing or stale Dolt `origin` remote to the current checkout's
   host-path beads worktree remote before Dolt sync. The repair updates the
   SQL Dolt remote directly rather than writing `sync.remote`, so it cannot
   commit host-local absolute paths into `.beads/config.yaml`. Sandbox/container
   invocations use the same current-checkout remote only as a temporary sync
   override and restore the prior `origin` before exit, so they do not leave
   `/workspace` paths in shared config.
10. **Context-aware invocation** — `wrix beads push` is safe to invoke
   unconditionally. Under `$LOOM_INSIDE` it is a full no-op (exit 0); when
   the git root is unresolvable it fails fast with an actionable message;
   otherwise it runs the dolt-sync and beads-branch sync. Consumers need no
   `$LOOM_INSIDE` guard around the call.
11. **Beads-worktree resilience** — the beads-branch sync recreates the
   beads worktree when it is absent or present-but-invalid (dangling gitdir
   after the admin directory was pruned/removed), rebuilding relative to the
   current `$ROOT` so host and container paths stay correct; and every git
   invocation in that section, including the recreating `git worktree add`,
   skips prek so the config-less `beads` branch does not abort the sync.
12. **Sandbox config staging** — sandbox launchers stage beads config and
   metadata only. They do not stage `.beads/issues.jsonl`, and they do not
   permit JSONL auto-import or embedded Dolt fallback when the Dolt service is
   unavailable.
13. **Sandbox Dolt remote mapping** — direct `bd dolt pull` / `bd dolt push`
   inside a sandbox temporarily remap Dolt `origin` to the container-visible
   `/workspace/.git/beads-worktrees/<branch>/.beads/dolt-remote` for the
   command and restore the persisted host-path remote afterwards, so the same
   database works from host and container contexts.

### Non-Functional

1. **Loud shellHook failure** — shellHook fails non-zero with a clear
   stderr message when prerequisites are missing or the Dolt server is
   unreachable. No fallback to embedded Dolt.
2. **Portability** — works on Linux (podman) and Darwin (Apple `container`
   or podman-via-VM) via the service lifecycle and Dolt endpoint surfaces
   defined by `services.md`.
3. **Conflict-free sync** — `wrix beads push` pushes before it pulls so the
   common case bypasses any merge; on the pull-fallback path it fails
   loud rather than silently overwriting `status` or `labels` (see
   *Session-Close Sync*).
4. **Caller-agnostic, fail-loud invocation** — `wrix beads push` never emits a
   bare `fatal: not a git repository: (null)`; an unresolvable repository
   produces an actionable non-zero error, and a loom-managed clone produces
   a clean no-op. Downstream session-close instructions reduce to an
   unconditional `wrix beads push` step.

## Out of Scope

- `bd` CLI implementation (external upstream tool)
- Web UI for issue management
- Integration with external trackers (Jira, Linear)
- Lifecycle management of the process that triggers shellHook — beads
  ensures its own container is independent; what the caller does is the
  caller's concern
- Downstream consumers' session-close documentation (e.g. another repo's
  `AGENTS.md` land-the-plane block) — `wrix beads push` owns the context-handling
  behavior so consumers invoke it unconditionally; how each repo documents
  that call is the repo's concern
- Publication of `main` + `beads` from inside a loom-managed bead clone —
  the loom driver owns that after a Clean review verdict; `wrix beads push`
  deliberately no-ops under `$LOOM_INSIDE` rather than publishing
- Container runtime selection (covered by `sandbox.md`)
- Non-beads services in `<repo>-service` (covered by `services.md`)
- Opt-in / legacy ordering flag for `wrix beads push` — push-before-pull is
  the unconditional default
- Conflict detection on columns other than `status` and `labels`
- Insert / delete row collisions during the pull-fallback merge (the
  intent-protection check covers updates only)
- One-shot cleanup of historical `gc:session`-labelled rows and their
  events in the Dolt database — operator concern, not part of
  `wrix beads push`'s recurring responsibilities
- Per-invocation suppression of bd auto-export via env var or flag —
  current bd has no such opt-out; `wrix beads push` writes the persistent
  config-file setting instead
