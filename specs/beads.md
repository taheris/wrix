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

Beads is an external CLI (`bd`) backed by a Dolt SQL database. Wrapix
provides:

- A **per-workspace Dolt container** (`<basename>-beads`) serving
  `.beads/dolt` on a workspace-hashed port. One container per workspace path,
  reused across invocations; the name and port derive from
  `sha256(workspace path)` so concurrent workspaces do not collide.
- A **shellHook** that ensures the container is running and exports
  `BEADS_DOLT_SERVER_*` env vars so `bd` connects through the container
  rather than embedding Dolt in-process.
- **CLI bundles** on the devShell PATH: `beads-dolt` (container manager —
  `start`/`stop`/`status`/`port`/`name`/`socket`/`host`/`attach`) and
  `beads-push` (session-close branch sync).

### Container lifecycle isolation

The Dolt container started by `beads.shellHook` must have a lifecycle
independent of the process that triggered shellHook evaluation. Stopping or
restarting the caller (a shell, an editor process, a systemd user service)
must not block on container teardown, nor cause the container to be SIGKILLed
as a side effect of the caller's stop timeout.

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

## CLI Surface

`bd …` commands are upstream beads; `beads-push` is the wrapix-provided
session-close wrapper.

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
| `beads-push` | Session-close sync: commit the Dolt remote and push the `beads` git branch (see *Session-Close Sync*) |

Issue types: task, bug, feature, epic, question, docs. Priority levels
range from 0 (critical) to 4 (backlog); the `P0`–`P4` form is accepted
as an alias.

## Storage

Beads splits state across the main worktree and a dedicated `beads`
branch worktree:

- **Main worktree `.beads/`** holds the repository config (`config.yaml`)
  and database metadata (`metadata.json`) — the only files git tracks
  under `.beads/`. Everything else is gitignored: the local Dolt database
  (`dolt/`, served by the per-workspace Dolt container), the JSONL backup
  (`backup/`), and runtime state (`bd.sock`, lock/log files, sync state).
- **Beads branch worktree** at `.git/beads-worktrees/beads/.beads/` holds
  `dolt-remote/`, the canonical Dolt remote committed on the `beads`
  branch.

`bd dolt pull` / `bd dolt push` move data between the local `dolt/` and
the remote (on disk in the beads branch worktree).
`bd dolt push` alone does not move the `beads` git branch to GitHub —
that's the role of `beads-push` (see *Session-Close Sync*).

The shellHook exports the connection info `bd` uses to reach the Dolt
server: `BEADS_DOLT_SERVER_SOCKET` on Linux, `BEADS_DOLT_SERVER_HOST` and
`BEADS_DOLT_SERVER_PORT` on Darwin.

## Configuration

Key settings in `.beads/config.yaml`:

| Setting | Purpose |
|---------|---------|
| `issue-prefix` | Prefix for issue IDs (e.g., "wx" → "wx-1") |
| `sync-branch` | Git branch for beads data |
| `sync.mode` | Sync mode: `dolt-native` |
| `export.auto` | bd JSONL auto-export toggle; set `false` by `beads-push` (see *Auto-export suppression*) |

## Session-Close Sync

`beads-push` is the session-close synchronization step: it lands local
operator state (`bd close`, `bd update --status=…`, label changes) into
the on-disk Dolt remote, then pushes the `beads` git branch to GitHub.

### Ordering

`beads-push` attempts `bd dolt push` before `bd dolt pull`. When the
push succeeds, the local state reaches the remote verbatim and no merge
is computed. Only when the push fails because the remote has advanced
(a fast-forward rejection from Dolt) does `beads-push` fall back to
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

On the pull-fallback path, `beads-push` must not silently overwrite
local operator state. Before `bd dolt pull`, it records
`(issue_id, intended_status, intended_labels)` for every row where the
local-ahead-of-remote commits modified `status` or `labels` (sourced
from a Dolt system-table column diff between local HEAD and the remote
tracking branch). After the pull, it re-reads those rows; any row
whose post-pull value diverges from the recorded intent causes
`beads-push` to exit non-zero with the affected issue IDs in stderr.
No push is attempted. The operator resolves the conflict by hand.

### Auto-export suppression

`beads-push` disables bd's auto-export hook
(`bd config set export.auto false`) on every invocation. The hook is
redundant with the explicit `bd dolt` calls the script already makes,
and its `git add .beads/issues.jsonl` produces noisy warnings in any
repo that gitignores the JSONL (the common case post-Dolt, including
wrapix itself). The write is idempotent and persists in
`.beads/config.yaml`, so subsequent bd calls in that repo skip
auto-export until a consumer explicitly re-enables it.

### Pre-pull cleanup

Before `git pull --rebase` in the beads worktree, `beads-push` commits
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

- `bd dolt pull` and `bd dolt push` succeed inside the wrapix sandbox using
  the host-mounted `.beads/` directory, with no fallback to a per-container
  embedded Dolt
  [judge](../tests/judges/beads.sh#test_sync_in_container)

- `beads.shellHook` launches the Dolt container with a lifecycle independent
  of the caller, so stopping a long-running parent (e.g. a `systemd --user`
  service that triggered shellHook evaluation via envrc) does not block on
  container teardown nor deliver SIGKILL to the container as a side effect
  of the caller's stop timeout
  [judge](../tests/judges/beads.sh#test_shellhook_lifecycle_isolation)

- The same workspace path yields the same container name and port across
  `beads.shellHook` invocations; different workspace paths yield different
  names and ports
  [judge](../tests/judges/beads.sh#test_workspace_naming_determinism)

- `beads.shellHook` fails non-zero with a stderr message when no container
  runtime is available or when Dolt does not become reachable within the
  startup budget — no fallback to embedded Dolt
  [judge](../tests/judges/beads.sh#test_shellhook_fail_loud)

- `beads-push` attempts `bd dolt push` before `bd dolt pull`, so a
  session-close run against an up-to-date remote never enters the Dolt
  merge path
  [judge](../tests/judges/beads.sh#test_beadspush_pushes_before_pulls)

- On the pull-fallback path, `beads-push` snapshots local `status` and
  `labels` intent before pulling and exits non-zero with the affected
  issue IDs in stderr when the post-pull row state diverges from that
  intent — no push attempted, no silent overwrite
  [judge](../tests/judges/beads.sh#test_beadspush_failloud_on_intent_overwrite)

- `beads-push` disables bd's auto-export hook on every invocation
  (idempotent), leaving `export.auto: false` persisted in
  `.beads/config.yaml`, so subsequent bd calls inside and outside
  `beads-push` no longer emit the `Warning: auto-export: git add failed`
  message or write `.beads/issues.jsonl`
  [judge?](../tests/judges/beads.sh#test_beadspush_disables_autoexport)

- `beads-push`'s pre-pull cleanup commits any pre-existing dirt in the
  beads worktree — untracked files OR modified tracked files left by a
  previously-interrupted run — using the same detection surface
  `git rebase` itself consults, so the subsequent `git pull --rebase`
  never aborts with "You have unstaged changes"
  [judge?](../tests/judges/beads.sh#test_beadspush_pre_pull_cleanup_canonical)

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
5. **Per-workspace Dolt container** — `beads.shellHook` derives a
   deterministic container name and port from the workspace path
   (sha256-based) so concurrent workspaces do not collide.
6. **Lifecycle isolation** — the container started by `beads.shellHook`
   has a lifecycle independent of the process that triggered its
   evaluation.
7. **Session-close sync** — `beads-push` attempts `bd dolt push` before
   `bd dolt pull`; on the pull-fallback path it snapshots local `status`
   and `labels` intent before pulling and refuses to overwrite divergent
   rows.
8. **Auto-export suppression** — `beads-push` disables bd's auto-export
   hook on every invocation (idempotent); the pre-pull cleanup in the
   beads worktree uses `git status --porcelain` so any dirt the rebase
   would refuse is committed first.

### Non-Functional

1. **Loud shellHook failure** — shellHook fails non-zero with a clear
   stderr message when prerequisites are missing or the Dolt server is
   unreachable. No fallback to embedded Dolt.
2. **Portability** — works on Linux (podman) and Darwin (Apple `container`
   or podman-via-VM) via the same `beads-dolt` CLI surface.
3. **Conflict-free sync** — `beads-push` pushes before it pulls so the
   common case bypasses any merge; on the pull-fallback path it fails
   loud rather than silently overwriting `status` or `labels` (see
   *Session-Close Sync*).

## Out of Scope

- `bd` CLI implementation (external upstream tool)
- Web UI for issue management
- Integration with external trackers (Jira, Linear)
- Lifecycle management of the process that triggers shellHook — beads
  ensures its own container is independent; what the caller does is the
  caller's concern
- Container runtime selection (covered by `sandbox.md`)
- Opt-in / legacy ordering flag for `beads-push` — push-before-pull is
  the unconditional default
- Conflict detection on columns other than `status` and `labels`
- Insert / delete row collisions during the pull-fallback merge (the
  intent-protection check covers updates only)
- One-shot cleanup of historical `gc:session`-labelled rows and their
  events in the Dolt database — operator concern, not part of
  `beads-push`'s recurring responsibilities
- Per-invocation suppression of bd auto-export via env var or flag —
  current bd has no such opt-out; `beads-push` writes the persistent
  config-file setting instead
