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

- A **per-workspace dolt container** (`<basename>-beads`) serving
  `.beads/dolt` on a workspace-hashed port. One container per workspace path,
  reused across invocations; the name and port derive from
  `sha256(workspace path)` so concurrent workspaces do not collide.
- A **shellHook** that ensures the container is running and exports
  `BEADS_DOLT_SERVER_*` env vars so `bd` connects through the container
  rather than embedding dolt in-process.
- **CLI bundles** on the devShell PATH: `beads-dolt` (container manager —
  `start`/`stop`/`status`/`port`/`name`/`socket`/`host`/`attach`) and
  `beads-push` (session-close branch sync).

### Container lifecycle isolation

The dolt container started by `beads.shellHook` must have a lifecycle
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

| Command | Purpose |
|---------|---------|
| `bd ready` | Show issues ready to work (no blockers) |
| `bd list --status=<state>` | List issues by status |
| `bd show <id>` | Issue details with dependencies |
| `bd create --title=… --type=task --priority=N` | Create issue (priority 0-4) |
| `bd update <id> --status=in_progress` | Claim work |
| `bd update <id> --add-label=<l>` / `--remove-label=<l>` | Manage labels |
| `bd update <id> --notes=…` | Set issue notes |
| `bd close <id>` | Close issue |
| `bd dep add <issue> <depends-on>` | Add dependency |
| `bd dolt pull` / `bd dolt push` | Sync the Dolt database via the Dolt remote |
| `beads-push` | Session-close sync: commit the Dolt remote and push the `beads` git branch (see *Storage*) |

Issue types: task, bug, feature, epic, question, docs. Priority levels: P0
(critical) through P4 (backlog).

## Storage

Beads splits state across two worktrees:

- **Main worktree `.beads/`** holds the repository config (`config.yaml`)
  and database metadata (`metadata.json`) — the only files git tracks
  under `.beads/`. Everything else is gitignored: the local Dolt database
  (`dolt/`, served by the per-workspace dolt container), the JSONL backup
  (`backup/`), and runtime state (`bd.sock`, daemon files, sync state).
- **Beads branch worktree** at `.git/beads-worktrees/beads/.beads/` holds
  `dolt-remote/`, the canonical Dolt remote committed on the `beads`
  branch.

`bd dolt pull` / `bd dolt push` move data between the local `dolt/` and
the remote (which is on disk in the beads branch worktree). `beads-push`
then commits that remote and pushes the `beads` git branch to the GitHub
remote — `bd dolt push` alone does not move the git branch, so session
close must run `beads-push`.

The shellHook exports the connection info `bd` uses to reach the dolt
server: `BEADS_DOLT_SERVER_SOCKET` on Linux, `BEADS_DOLT_SERVER_HOST` and
`BEADS_DOLT_SERVER_PORT` on Darwin.

## Configuration

Key settings in `.beads/config.yaml`:

| Setting | Purpose |
|---------|---------|
| `issue-prefix` | Prefix for issue IDs (e.g., "wx" → "wx-1") |
| `sync-branch` | Git branch for beads data |
| `sync.mode` | Sync mode: `dolt-native` |
| `federation.remote` | Dolt remote URL for container sync |

## Loom Integration

Beads is the persistence layer for loom's molecule state. Bead rows hold
issue state, dependency edges, labels, and notes that loom subcommands
read and write:

- `loom plan` writes implementation notes via `loom note set` (backed by
  `bd update --notes`); later sessions (`loom todo`, `loom run`) read them.
- `loom todo` creates beads from spec changes via `bd create` and links
  dependencies via `bd dep add`.
- `loom run` claims a bead via `bd update --status=in_progress`, completes
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
  embedded dolt
  [judge](../tests/judges/beads.sh#test_sync_in_container)

- `beads.shellHook` launches the dolt container with a lifecycle independent
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
  runtime is available or when dolt does not become reachable within the
  startup budget — no fallback to embedded dolt
  [judge](../tests/judges/beads.sh#test_shellhook_fail_loud)

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
5. **Per-workspace container** — `beads.shellHook` derives a deterministic
   container name and port from the workspace path (sha256-based) so
   concurrent workspaces do not collide.
6. **Lifecycle isolation** — the container started by `beads.shellHook`
   has a lifecycle independent of the process that triggered shellHook
   evaluation.

### Non-Functional

1. **Loud failure** — shellHook fails non-zero with a clear stderr message
   when prerequisites are missing or the dolt server is unreachable. No
   fallback to embedded dolt.
2. **Portability** — works on Linux (podman) and Darwin (Apple `container`
   or podman-via-VM) via the same `beads-dolt` CLI surface.
3. **Conflict-free sync** — Dolt-native merge handles concurrent edits.

## Out of Scope

- `bd` CLI implementation (external upstream tool)
- Web UI for issue management
- Integration with external trackers (Jira, Linear)
- Lifecycle management of the process that triggers shellHook — beads
  ensures its own container is independent; what the caller does is the
  caller's concern
- Container runtime selection (covered by `sandbox.md`)
