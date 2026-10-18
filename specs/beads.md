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

Beads is an external CLI (`bd`) backed by a Dolt SQL database. Wrix provides:

- The shared service lifecycle and identity contract defined by `services.md`.
  Beads consumes its workspace namespace's published Dolt endpoint and managed
  database/sync-worktree storage.
- A **shellHook** that ensures the service container is running and exports
  `BEADS_DOLT_SERVER_*` env vars so `bd` connects through the Dolt service
  rather than embedding Dolt in-process.
- **Wrix CLI integration** on the devShell PATH: `cli.md` owns the root command
  grammar; `services.md` owns `wrix service ...` service lifecycle and Dolt
  endpoint diagnostics; this spec owns bare `wrix beads` information and
  `wrix beads push` session-close branch sync.
- A validated issue-identifier boundary between Dolt query output and the
  pull-fallback workflow, so snapshot queries and diagnostics cannot consume
  malformed raw identifiers.

## Command Surface

`bd …` commands are upstream beads. `cli.md` owns Wrix command placement; this
spec owns bare `wrix beads` information and explicit `wrix beads push`
session-close synchronization.

Bare `wrix beads` reports non-secret selected-workspace configuration, its
configured/defaulted sync branch and existing managed sync-worktree/registration
information. Missing or stale state is reported, not initialized or recovered;
configuration alone does not prove database readiness. Inspection does not query
SQL or synchronize, change export policy, create/repair worktrees or databases,
register/start/repair services or issue credentials. `wrix beads push` remains
an explicit action with its existing context guards and sync behavior.
Acceptance: [Information Acceptance](#information-acceptance).

| Command                                                         | Purpose                                                         | Verifier                                          |
| --------------------------------------------------------------- | --------------------------------------------------------------- | ------------------------------------------------- |
| `bd ready`                                                      | Show issues ready to work (no blockers)                         | [system](test-ci:test-beads-live-system)          |
| `bd list --status=<state>`                                      | List issues by status                                           | [system](test-ci:test-beads-live-system)          |
| `bd show <id>`                                                  | Issue details with dependencies                                 | [system](test-ci:test-beads-live-system)          |
| `bd create --title=… --type=task --priority=N`                  | Create an issue                                                 | [system](test-ci:test-beads-live-system)          |
| `bd update <id> --status=in_progress`                           | Claim work                                                      | [system](test-ci:test-beads-live-system)          |
| `bd update <id> --add-label=<label>` / `--remove-label=<label>` | Manage labels                                                   | [system](test-ci:test-beads-live-system)          |
| `bd update <id> --notes=…`                                      | Set issue notes                                                 | [system](test-ci:test-beads-live-system)          |
| `bd close <id>`                                                 | Close an issue                                                  | [system](test-ci:test-beads-live-system)          |
| `bd dep add <issue> <depends-on>`                               | Add a dependency                                                | [system](test-ci:test-beads-live-system)          |
| `bd dolt pull` / `bd dolt push`                                 | Sync the local Dolt database against the remote                 | [system](test-ci:test-beads-live-system)          |
| `wrix beads`                                                    | Read selected-workspace and sync configuration without mutation | [Information Acceptance](#information-acceptance) |
| `wrix beads push`                                               | Synchronize session-close state and the configured sync branch  | [system](test-ci:test-beads-live-system)          |

Issue types are task, bug, feature, epic, chore, and decision. Priority levels
range from 0 (critical) to 4 (backlog); the `P0`–`P4` form is accepted as an
alias [system](test-ci:test-beads-live-system).

## Storage

Acceptance: [Managed Storage Acceptance](#managed-storage-acceptance).

The repository's main `.beads/` retains tracked ignore policy, `config.yaml` and
`metadata.json`, plus non-authoritative backup/runtime artifacts. Live Dolt
state belongs to the workspace namespace beneath the service-visible data root
defined by `services.md`, not to a repository-local `.beads/dolt` directory.

Within that managed namespace:

- `dolt/` contains the authoritative live workspace database.
- `beads-sync/<branch>/` is the canonical worktree of this repository's
  configured sync branch; its `.beads/dolt-remote/` is the file remote committed
  and published through Git.

The repository retains its Git branch/admin ownership. The service can access
the database and remote without repository/home mounts. Host Git uses the
managed worktree; sandbox Git receives only its own narrowly mapped worktree and
uses context-correct repository/admin coordinates without rewriting shared Git
pointers for another caller.

Service registration establishes a canonical namespace-local Dolt `origin`. Host
and sandbox `bd dolt pull/push` use that same remote, without temporary
host-versus-container path overrides. Incorrect origins cannot redirect sync
outside the selected namespace. Remote mutation and Git sync coordinate through
a workspace-scoped lock, never a host-wide serialization lock.

The shellHook exports the namespace-bound endpoint and scoped SQL authorization
from `services.md`: Unix socket on Linux when available, shared-role TCP on
Darwin. Scoped SQL credentials and database/file-remote selection enforce the
namespace; a distinct workspace TCP port is not required. Controller routing and
authenticated native SQL/file isolation follow
[Shared Role Endpoints](services.md#shared-role-endpoints). Sandbox staging
remains limited to public beads config/metadata; it does not import JSONL or
expose another workspace's managed data. Missing registered state or endpoints
fail loudly rather than creating an unrelated empty database, embedding Dolt, or
importing backups.

Host-managed migration preserves existing issue contents, Dolt history and
remotes, configured sync branch, branch commits and uncommitted remote contents.
It relocates live database and canonical worktree together, validates the new
view before switching clients, coordinates concurrent users, and never exposes
two independently writable authoritative databases. Interrupted migration is
recoverable without overwriting newer state. Normal operation does not maintain
a second synchronized remote snapshot merely to preserve the old layout.

## Configuration

Key settings in `.beads/config.yaml`:

| Setting        | Purpose                                                                                       | Verifier                                 |
| -------------- | --------------------------------------------------------------------------------------------- | ---------------------------------------- |
| `issue-prefix` | Prefix for issue IDs (e.g., "wx" → "wx-1")                                                    | [system](test-ci:test-beads-live-system) |
| `sync-branch`  | Git branch for beads data                                                                     | [system](test-ci:test-beads-live-system) |
| `sync.mode`    | Sync mode: `dolt-native`                                                                      | [system](test-ci:test-beads-live-system) |
| `export.auto`  | bd JSONL auto-export toggle; set `false` by `wrix beads push` (see _Auto-export suppression_) | [system](test-ci:test-beads-live-system) |

## Session-Close Sync

`wrix beads push` is the session-close synchronization step: it lands local
operator state (`bd close`, `bd update --status=…`, label changes) into the
on-disk Dolt remote, then pushes the configured sync branch to GitHub.

### Invocation contexts

`wrix beads push` is safe to invoke unconditionally — it detects its context and
acts accordingly rather than requiring consumers to guard the call. A consumer's
session-close step is therefore an unconditional `wrix beads push`; no
`$LOOM_INSIDE` check and no manual dolt-sync-race workaround belong in
downstream "land the plane" instructions.

- **Loom-managed bead clone (`$LOOM_INSIDE` set).** Full no-op: no git or dolt
  operation runs and `wrix beads push` exits 0 with a one-line notice. Inside a
  loom clone `origin` points at the driver workdir (not GitHub); the canonical
  managed beads worktree and publication belong to the driver, not the worker.
  The loom driver publishes `main` + `beads` after a Clean review verdict, and
  the worker's scoped `bd` writes already reach the authoritative database.
  Running `bd dolt push` here would target the wrong remote and add a second
  writer racing the driver, so `wrix beads push` declines entirely.
- **Unresolvable repository (`$LOOM_INSIDE` unset).** When
  `git rev-parse --show-toplevel` does not resolve a workspace root,
  `wrix beads push` fails fast with an actionable stderr message naming the
  unresolved repository, rather than letting an empty `ROOT` flow into a git
  invocation that prints `fatal: not a git repository: (null)`.
- **Normal host session.** The dolt-sync (see _Ordering_) and beads-branch sync
  run as described below, including the existing skip when no `<branch>`
  resolves locally or on `origin`.

This context guard runs first — before the auto-export write and any `bd dolt`
call — so a loom-clone invocation has zero side effects.

### Ordering

`wrix beads push` attempts `bd dolt push` before `bd dolt pull`. When the push
succeeds, the local state reaches the remote verbatim and no merge is computed.
Only when the push fails because the remote has advanced (a fast-forward
rejection from Dolt) does `wrix beads push` fall back to pull-then-push. Other
push failures (network, auth, disk) propagate as-is rather than triggering the
fallback.

Rationale: an interior `pull` ahead of `push` runs Dolt's default three-way
merge over rows the local session has just written. Dolt's default policy can
pick the remote side over the local side without surfacing a conflict, silently
reverting operator state (e.g. a `bd close` returning to `blocked`) before the
push that would have landed it. Pushing first ensures the local commits become
the merge base for any subsequent remote activity rather than being merged
against pre-write remote state.

### Pull-fallback intent protection

On the pull-fallback path, `wrix beads push` must not silently overwrite local
operator state. Before `bd dolt pull`, it records
`(issue_id, intended_status, intended_labels)` for every row where the
local-ahead-of-remote commits modified `status` or `labels` (sourced from a Dolt
system-table column diff between local HEAD and the remote tracking branch).
After the pull, it re-reads those rows; any row whose post-pull value diverges
from the recorded intent causes `wrix beads push` to exit non-zero with the
affected issue IDs in stderr. No push is attempted. The operator resolves the
conflict by hand.

### Auto-export suppression

`wrix beads push` disables bd's auto-export hook
(`bd config set export.auto false`) on every invocation that proceeds past the
context guard (a `$LOOM_INSIDE` no-op runs nothing at all). The hook is
redundant with the explicit `bd dolt` calls the script already makes, and its
`git add .beads/issues.jsonl` produces noisy warnings in any repo that
gitignores the JSONL (the common case post-Dolt, including wrix itself). The
write is idempotent and persists in `.beads/config.yaml`, so subsequent bd calls
in that repo skip auto-export until a consumer explicitly re-enables it.

### Beads-worktree resilience

The beads-branch sync recreates the beads worktree when it is absent **or**
present-but-invalid. The recreate guard fires both when the worktree directory
does not exist and when it exists but is no longer a valid git worktree — e.g.
its admin directory `.git/worktrees/<branch>` was pruned or removed, leaving a
dangling gitdir pointer. In the invalid case `wrix beads push` prunes the stale
worktree registration, removes the directory, and re-adds the worktree relative
to the current repository's managed workspace namespace (falling back to
`origin/<branch>`, or skipping with a notice when no branch exists). Host and
sandbox views retain correct Git admin/worktree mapping without moving the
canonical files back into `.git/` or leaving caller-specific pointers in shared
state.

Recovery preserves the canonical Dolt remote contents and the sync branch's
commits. An invalid worktree is not treated as an alternate source of bead
state, and successful recovery leaves the branch ready for the same pull,
commit, and push behavior as an existing valid worktree.

The sync branch is parsed as a relative Git branch name before mutation.
Recovery rejects symlinked components in the repository's Git administration and
the selected managed workspace/worktree/staged-remote paths. It checks these
paths before mutation and again after checkout, so a symlinked `.beads`
directory in the sync branch cannot redirect restoration into unrelated data.
When restoration is unsafe, the staged Dolt remote remains available for
operator recovery.

Every git invocation in the beads-branch sync — including the `git worktree add`
that recreates the worktree — uniformly skips prek, because the sync branch
legitimately carries no prek config and a post-checkout hook firing there would
abort the sync with `No prek.toml … found`. The prek-skip is scoped to this
section; the preceding Dolt commit/push phase is unchanged.

### Pre-pull cleanup

Before `git pull --rebase` in the beads worktree, `wrix beads push` commits any
pre-existing dirt (untracked files, modified tracked files) from prior
interrupted runs that would otherwise abort the rebase. The detection surface is
the same one `git rebase` itself consults — `git status --porcelain` after
`git update-index --refresh` — so a stale stat cache cannot hide a change from
the cleanup that the rebase will then refuse on.

## Loom Integration

Beads is the persistence layer for loom's molecule state. Bead rows hold issue
state, dependency edges, labels, and notes that loom subcommands read and write:

- `loom plan` writes implementation notes via `loom note set` (backed by
  `bd update --notes`); later sessions (`loom todo`, `loom loop`) read them.
- `loom todo` creates beads from spec changes via `bd create` and links
  dependencies via `bd dep add`.
- `loom loop` claims a bead via `bd update --status=in_progress`, completes the
  work, and closes via `bd close`.
- `loom gate verify` enumerates a bead's acceptance criteria; on `loom:blocked`
  / `loom:clarify` labels the bead waits for `loom msg` resolution.
- `loom msg` reads and responds to labelled beads via
  `bd list --label=loom:clarify`, `bd update --remove-label=…`, and writes
  Options Format Contract content into the notes field.

Beads owns the lifecycle primitives; loom owns the policy on top of them. The
Options Format Contract for `loom:clarify` notes is defined by loom upstream,
not by this spec.

## Success Criteria

- YAML sync-branch parsing supports quoted strings, escapes, comments, flow
  mappings, and aliases
  [test](../crates/wrix-core/src/beads_config/mod.rs::sync_branch_parses_yaml_quotes_comments_flow_and_aliases)
- An absent sync branch defaults to `beads`, without treating nested unrelated
  fields as configuration
  [test](../crates/wrix-core/src/beads_config/mod.rs::absent_branch_defaults_without_reading_nested_fields)
- Beads and service planning share YAML sync-branch parsing
  [test](../crates/wrix-service/src/lifecycle/mod.rs::service_plan_retains_the_shared_yaml_branch)
- Malformed YAML, non-string branches, and duplicate branch fields fail before
  beads synchronization
  [test](../crates/wrix-cli/tests/beads_push.rs::malformed_yaml_sync_branch_is_rejected_before_mutation)
- Malformed YAML, non-string branches, and duplicate branch fields fail before
  service layout changes
  [test](../crates/wrix-service/src/lifecycle/mod.rs::service_plan_rejects_invalid_yaml_before_layout_changes)

- Service startup persists a local server-only policy with automatic startup,
  JSONL import, and JSONL export disabled. Host checkout hooks honor that policy
  outside a development shell while still running unrelated chained hooks
  [test](../crates/wrix-cli/tests/service_lifecycle.rs::managed_checkout_outside_devshell_skips_import_and_preserves_chained_hooks)

- Managed-policy installation preserves unrelated local configuration, database
  identity, `sync.mode`, and `sync-branch`
  [test](lifecycle::managed::test::managed_policy_preserves_sync_identity_and_unrelated_local_settings)

- Both sandbox entrypoints authenticate a read-only SQL query from the guest
  before executing the agent; unavailable endpoints fail without automatic
  database startup or JSONL import [system](verify:beads.sandbox-readiness)

- Concurrent TCP and Unix-socket clients read the same existing issue data
  without changing the configured file remote or starting another Dolt server
  [system](verify:beads.shared-access)

- An unavailable database during remote discovery aborts `wrix beads push`
  without attempting remote repair
  [test](../crates/wrix-cli/tests/beads_push.rs::unavailable_database_does_not_trigger_remote_repair)

- A canonical managed namespace `file://` remote remains unchanged during normal
  host and sandbox synchronization
  [test?](../crates/wrix-cli/tests/beads_managed_storage.rs::canonical_remote_is_stable_across_caller_contexts)

- Git tracks exactly `.beads/.gitignore`, `.beads/config.yaml`, and
  `.beads/metadata.json` under the main worktree's `.beads/` directory
  [check](verify:beads.tracked-files)

- `bd dolt pull` and `bd dolt push` succeed inside the wrix sandbox using staged
  beads config plus the shared Dolt service, with no fallback to a per-container
  embedded Dolt or JSONL import [system](test-ci:test-beads-live-system)

- Direct host and Linux sandbox `bd dolt pull/push` reach the same canonical
  managed remote without path remapping or a second authoritative database.
  [system?](test-ci:test-beads-managed-storage-system)

- Direct Darwin sandbox `bd dolt pull/push` use the same managed remote and
  correctly scoped database endpoint without host-path remapping.
  [system?](verify:beads.darwin-managed-remote)

- `beads.shellHook` fails non-zero with a stderr message on Linux and Darwin
  when the selected container runtime is unavailable, before service startup
  [system](verify:beads.shellhook-runtime-fail-loud)

- `beads.shellHook` fails non-zero with a stderr message on Linux and Darwin
  when the platform-specific Dolt endpoint does not become reachable within the
  startup budget — no fallback to embedded Dolt
  [system](verify:beads.shellhook-endpoint-fail-loud)

- On Darwin, `beads.shellHook` selects podman when the default Apple `container`
  runtime is unavailable and podman is present
  [system](verify:beads.shellhook-darwin-runtime-fallback)

- The Linux launcher stages beads config/metadata but not `.beads/issues.jsonl`
  or other workspace beads files for sandboxed clients
  [test](../crates/wrix-cli/tests/sandbox_launch.rs::launcher_stages_only_beads_config_and_metadata)

- A missing Dolt endpoint fails loudly instead of triggering JSONL auto-import
  or embedded Dolt recovery [system](verify:beads.no-embedded-fallback)

- `wrix beads push` attempts `bd dolt push` before `bd dolt pull`, so a
  session-close run against an up-to-date remote never enters the Dolt merge
  path [test](../crates/wrix-cli/tests/beads_push.rs::push_precedes_pull)

- Authentication and permission failures during Dolt push propagate without
  pulling
  [test](../crates/wrix-cli/tests/beads_push.rs::authentication_and_permission_failures_never_pull)

- On the pull-fallback path, `wrix beads push` snapshots local `status` and
  `labels` intent before pulling and exits non-zero with the affected issue IDs
  in stderr when the post-pull row state diverges from that intent — no push
  attempted, no silent overwrite [system](test-ci:test-beads-live-system)

- `wrix beads push` disables bd's auto-export hook on every invocation that
  proceeds past the context guard (idempotent), leaving `export.auto: false`
  persisted in `.beads/config.yaml`, so subsequent bd calls inside and outside
  `wrix beads push` no longer emit the `Warning: auto-export: git add failed`
  message or write `.beads/issues.jsonl`
  [system](test-ci:test-beads-live-system)

- Trusted registration repairs a missing/stale remote only to the selected
  namespace's canonical file remote, never an arbitrary caller-supplied path
  [test?](../crates/wrix-cli/tests/beads_managed_storage.rs::remote_initialization_is_namespace_scoped)

- Host and sandbox invocations share canonical service-local remote coordinates
  without leaving caller-specific `/workspace` or host-path origins
  [test?](../crates/wrix-cli/tests/beads_managed_storage.rs::sync_does_not_rewrite_origin_for_caller_paths)

- When `$LOOM_INSIDE` is set, `wrix beads push` performs no git or dolt
  operation and exits 0 with a one-line notice, so a consumer may invoke it
  unconditionally inside a loom-managed bead clone — where `origin` points at
  the driver workdir and beads publication belongs to the driver — without error
  and without a second publisher racing the driver
  [test](../crates/wrix-cli/tests/beads_push.rs::loom_inside_is_noop)

- When `$LOOM_INSIDE` is unset and `git rev-parse --show-toplevel` does not
  resolve a workspace root, `wrix beads push` exits non-zero with an actionable
  stderr message naming the unresolved repository — never proceeding with an
  empty `ROOT` into a git invocation that prints
  `fatal: not a git repository: (null)`
  [test](../crates/wrix-cli/tests/beads_push.rs::missing_repo_fails_before_git_sync)

- `wrix beads push`'s pre-pull cleanup commits any pre-existing dirt in the
  beads worktree — untracked files OR modified tracked files left by a
  previously-interrupted run — using the same detection surface `git rebase`
  itself consults, so the subsequent `git pull --rebase` never aborts with "You
  have unstaged changes"
  [test](../crates/wrix-cli/tests/beads_push.rs::pre_pull_cleanup_uses_canonical_dirty_detection)

- An invalid managed sync worktree recovers its repository/admin mapping,
  preserves the canonical remote, and completes branch synchronization without
  invalid-repository errors
  [test?](../crates/wrix-cli/tests/beads_managed_storage.rs::managed_worktree_recovery_preserves_remote_and_admin_mapping)

- An absolute `sync-branch` is rejected before any `bd` operation or config
  write, without deleting unrelated filesystem data
  [test](../crates/wrix-cli/tests/beads_push.rs::absolute_sync_branch_is_rejected_before_mutation)

- Symlinked worktree and recovery directory components are rejected before
  mutation, preserving the external directory contents and original config
  [test](../crates/wrix-cli/tests/beads_push.rs::symlinked_managed_directories_are_rejected_before_mutation)

- Recovery refuses a symlinked `.beads` directory checked out from the sync
  branch without deleting external data or losing the staged Dolt remote
  [test](../crates/wrix-cli/tests/beads_push.rs::recovery_rejects_symlinked_remote_from_sync_branch)

- A hierarchical sync branch such as `team/beads` can recover an invalid
  worktree and complete sync while preserving its canonical Dolt remote
  [test](../crates/wrix-cli/tests/beads_push.rs::recovers_hierarchical_sync_branch_without_losing_dolt_remote)

- When the beads worktree and local sync branch are both absent but
  `origin/<branch>` exists, `wrix beads push` recreates an attached local branch
  that tracks `origin/<branch>` and completes the sync rather than attempting a
  rebase from detached HEAD
  [test](../crates/wrix-cli/tests/beads_push.rs::recovers_missing_worktree_from_origin_on_local_branch)

- Affected issue IDs returned by the Dolt diff query are parsed into validated
  identifiers before they reach snapshot SQL or diagnostics; malformed query
  values fail the workflow instead of becoming downstream strings
  [test](command::test::affected_id_output_rejects_malformed_query_values)

- Every git invocation in the sync-branch sync, including the `git worktree add`
  that recreates the worktree, skips prek, so a fresh worktree recreate
  completes without the caller setting `PREK_ALLOW_NO_CONFIG` and without a
  `No prek.toml … found` error from the config-less sync branch
  [test](../crates/wrix-cli/tests/beads_push.rs::git_sync_invocations_skip_prek)

### Information Acceptance

- Bare `wrix beads` reports non-secret selected-workspace configuration,
  configured/defaulted sync branch and existing managed-worktree/registration
  information, without presenting missing/stale metadata as database readiness.
  [test?](../crates/wrix-cli/tests/information_defaults.rs::bare_beads_reports_workspace_and_sync_configuration)
- Bare information does not query SQL, synchronize, change export policy,
  initialize/recover worktrees or databases, register/start/repair services or
  issue credentials, including with absent or stopped infrastructure.
  [test?](../crates/wrix-cli/tests/information_defaults.rs::bare_beads_information_never_starts_queries_or_repairs)

### Managed Storage Acceptance

- Migration relocates database and sync worktree while preserving issue data,
  Dolt history/remotes, branch commits and uncommitted remote contents.
  [test?](../crates/wrix-cli/tests/beads_managed_storage.rs::migration_preserves_database_history_and_sync_worktree)
- Interrupted or competing migration never overwrites newer state or exposes two
  writable authoritative databases.
  [test?](../crates/wrix-cli/tests/beads_managed_storage.rs::migration_is_recoverable_and_single_writer)
- Host and sandbox Git use their own namespace's canonical worktree with correct
  repository/admin mapping and no shared caller-specific pointer writes.
  [system?](test-ci:test-beads-managed-storage-system)
- Database/remote/Git-sync mutation uses workspace-scoped coordination, while
  independent workspaces can synchronize concurrently.
  [test?](../crates/wrix-cli/tests/beads_managed_storage.rs::sync_coordination_is_workspace_scoped)
- Missing registered data fails without empty replacement databases, embedded
  startup or backup import.
  [test?](../crates/wrix-cli/tests/beads_managed_storage.rs::missing_registered_data_never_creates_a_replacement)
- Migration/recovery validates both repository-admin and managed-root paths and
  rejects symlink escape while preserving recoverable remote contents.
  [test?](../crates/wrix-cli/tests/beads_managed_storage.rs::managed_paths_reject_symlink_escape_without_losing_remote)

## Requirements

### Functional

1. **Issue CRUD** — Create, read, update, and close issues via `bd`.
2. **Dependencies** — `bd dep add` records blocking relationships; `bd ready`
   excludes blocked issues.
3. **Labels and notes** — `bd update --add-label` / `--remove-label` / `--notes`
   provide the surfaces loom uses for its resolution loop and Options Format
   Contract content.
4. **Sync** — `bd dolt pull` / `bd dolt push` operate over the Dolt remote in
   the configured sync-branch worktree.
5. **Shared Dolt service** — `beads.shellHook` reaches its managed database
   through the namespace-scoped endpoint of the host-user service in
   `services.md`.
6. **Session-close sync** — `wrix beads push` attempts `bd dolt push` before
   `bd dolt pull`; on the pull-fallback path it snapshots local `status` and
   `labels` intent before pulling and refuses to overwrite divergent rows.
7. **Auto-export suppression** — `wrix beads push` disables bd's auto-export
   hook on every invocation past the context guard (idempotent); the pre-pull
   cleanup in the beads worktree uses `git status --porcelain` so any dirt the
   rebase would refuse is committed first.
8. **Canonical remote** — registration establishes the namespace-local file
   remote beside the managed sync worktree. Host/container commands use it
   without caller-specific remapping; invalid origins cannot escape the tenant.
9. **Context-aware invocation** — `wrix beads push` is safe to invoke
   unconditionally. Under `$LOOM_INSIDE` it is a full no-op (exit 0); when the
   git root is unresolvable it fails fast with an actionable message; otherwise
   it runs the dolt-sync and sync-branch sync. Consumers need no `$LOOM_INSIDE`
   guard around the call.
10. **Beads-worktree resilience** — the sync-branch sync recreates the beads
    worktree when it is absent or present-but-invalid (dangling gitdir after the
    admin directory was pruned/removed), rebuilding in the managed namespace
    with correct host/container Git admin mapping; and every git invocation in
    that section, including the recreating `git worktree add`, skips prek so the
    config-less sync branch does not abort the sync.
11. **Sandbox config staging** — sandbox launchers stage beads config and
    metadata only. They do not stage `.beads/issues.jsonl`, and they do not
    permit JSONL auto-import or embedded Dolt fallback when the Dolt service is
    unavailable.
12. **Managed storage** — live Dolt and the canonical sync worktree use the
    service-data namespace; migration preserves data/history/branch state and
    concurrent synchronization remains workspace-scoped.
13. **Information default** — bare `wrix beads` follows
    [Command Surface](#command-surface), with passive configuration inspection
    covered by [Information Acceptance](#information-acceptance).

### Non-Functional

1. **Loud shellHook failure** — shellHook fails non-zero with a clear stderr
   message when prerequisites are missing or the Dolt server is unreachable. No
   fallback to embedded Dolt.
2. **Portability** — works on Linux (podman) and Darwin (Apple `container` or
   podman-via-VM) via the service lifecycle and Dolt endpoint surfaces defined
   by `services.md`.
3. **Conflict-free sync** — `wrix beads push` pushes before it pulls so the
   common case bypasses any merge; on the pull-fallback path it fails loud
   rather than silently overwriting `status` or `labels` (see _Session-Close
   Sync_).
4. **Caller-agnostic, fail-loud invocation** — `wrix beads push` never emits a
   bare `fatal: not a git repository: (null)`; an unresolvable repository
   produces an actionable non-zero error, and a loom-managed clone produces a
   clean no-op. Downstream session-close instructions reduce to an unconditional
   `wrix beads push` step.

## Out of Scope

- `bd` CLI implementation (external upstream tool)
- Web UI for issue management
- Integration with external trackers (Jira, Linear)
- Lifecycle management of the process that triggers shellHook; `services.md`
  owns the service-container lifetime, while the caller owns its own process
- Downstream consumers' session-close documentation (e.g. another repo's
  `AGENTS.md` land-the-plane block) — `wrix beads push` owns the
  context-handling behavior so consumers invoke it unconditionally; how each
  repo documents that call is the repo's concern
- Publication of `main` + `beads` from inside a loom-managed bead clone — the
  loom driver owns that after a Clean review verdict; `wrix beads push`
  deliberately no-ops under `$LOOM_INSIDE` rather than publishing
- Container runtime selection (covered by `sandbox.md`)
- Non-beads services in the shared host-user container (covered by `services.md`
  and `proxy.md`)
- Opt-in / legacy ordering flag for `wrix beads push` — push-before-pull is the
  unconditional default
- Conflict detection on columns other than `status` and `labels`
- Insert / delete row collisions during the pull-fallback merge (the
  intent-protection check covers updates only)
- One-shot cleanup of historical `gc:session`-labelled rows and their events in
  the Dolt database — operator concern, not part of `wrix beads push`'s
  recurring responsibilities
