# Pre-commit Hooks

Staged git hooks via [prek](https://github.com/j178/prek) with a `flock`-serialized
shim to prevent concurrent stash races across agents sharing a workspace.

## Problem Statement

prek's stash/restore dance around unstaged changes can race with concurrent writers,
silently dropping working-tree edits (wx-m5yuq). Multiple wrapix agents on the same
workspace — and `loom run` quality-gate commits firing alongside human commits — re-expose
this race unless hook invocations serialize across processes.

## Requirements

### Hook Ownership

Hook stages are served from two locations:

- **`pre-commit` and `pre-push`** are served from `lib/prek/hooks/` via `core.hooksPath`
  (see FR2). The versioned shims there acquire the serialization `flock` and then
  `exec prek hook-impl …`. Git reads only from `core.hooksPath` for these stages, so
  anything under `.git/hooks/pre-commit` or `.git/hooks/pre-push` is inert.
- **All other stages** (`prepare-commit-msg`, `post-checkout`, `post-merge`) are served
  from `.git/hooks/`, written by `prek install`. Beads (bd) hooks run as prek-managed
  local hooks in `.pre-commit-config.yaml` via `bd hooks run <stage>`. The `.git/hooks/`
  directory is `chmod 555` to prevent `bd hooks install` from overwriting prek's shims
  for these non-FR2 stages.

To update the non-FR2 hook set:

```bash
chmod 755 .git/hooks/ && prek install -f && chmod 555 .git/hooks/
```

The FR2 shims are versioned in the repo and need no install step — they're effective as
soon as `core.hooksPath` is set (see FR2 *Install ordering*).

### Functional Requirements

#### FR1: prek Stage Separation

Configure `.pre-commit-config.yaml` with staged hooks:

| Stage | Hooks | Purpose |
|-------|-------|---------|
| pre-commit | treefmt, shellcheck, builtin hooks (trailing-whitespace, end-of-file-fixer, check-merge-conflict) | Fast validation on every commit |
| prepare-commit-msg | bd agent trailers | Add agent identity to commits |
| post-checkout | bd dolt pull | Pull Dolt state after branch switch |
| post-merge | bd dolt pull | Pull Dolt state after pull/merge |
| pre-push | nix flake check, loom integration tests | Slow validation before sharing |

#### FR2: Hook Serialization (`flock`)

The installed pre-commit and pre-push hooks acquire an exclusive `flock` on
`.wrapix/prek.lock` before invoking prek's hook run, and release it when prek exits.
Both stages are covered because concurrent commits and pushes both touch the same
working-tree stash window.

- **Mechanism** — versioned shims at `lib/prek/hooks/pre-commit` and
  `lib/prek/hooks/pre-push` source `lib/prek/lock.sh` for shared lock infrastructure,
  then call `_prek_acquire_lock` to serialize. The pre-commit shim `exec`s
  `prek hook-impl --hook-dir ... --script-version 4 --hook-type=pre-commit -- "$@"`
  (`hook-impl` is used rather than `prek run` because git passes positional args that
  `prek run` would mistake for hook/project selectors). The pre-push shim runs
  `prek run --stage pre-push` independently of git's SSH connection, then writes a
  stamp file (`.wrapix/push-verified`) on success; the user re-runs `git push` and the
  stamp is consumed instantly, pushing on a fresh connection. This avoids SSH idle
  timeouts during long test suites. Git is pointed at the shim directory via
  `core.hooksPath = lib/prek/hooks`.
- **Lock file** — `.wrapix/prek.lock`, gitignored, auto-created on first use; located
  in the main repo's `.wrapix/` so every agent (host, container with `.wrapix` mount,
  linked worktree) shares the same lock. A single lock covers both stages because both
  protect the same working-tree stash window — a commit and a push must not interleave
  either.
- **Path resolution from any worktree** — the shim resolves the lock path via
  `$(dirname "$(git rev-parse --git-common-dir)")/.wrapix/prek.lock`.
  `git rev-parse --git-common-dir` always returns the main repo's `.git/` from any
  linked worktree, so commits fired from a linked worktree share the same lock as the
  main repo — no relative-path surprises.
- **Timeout** — 600s (10 min) poll loop with dead-PID recovery. If the lock holder's
  PID is no longer running, the lock file is deleted and re-acquired on a fresh inode.
  If the timeout fires, the shim fails loudly (`>&2` with lock-holder PID) and exits
  non-zero so the commit or push aborts rather than deadlocking the workspace.
  Subprocesses (prek, nix) do not inherit the lock FD (`9>&-`) in the pre-push shim,
  preventing orphaned children from holding stale locks.
- **`--no-verify` bypass** — skips the hook and therefore the lock, but also skips
  prek's stash, so no race exists on that path. The documented escape hatch for
  emergency pushes.
- **`flock` availability** — `flock(1)` is a hard requirement, not a graceful-degrade.
  The shim aborts with a clear error (`flock not found — enter nix develop or install
  util-linux`) if `flock` is missing from `PATH`. `flock` is added to the nix devShell
  and to the wrapix sandbox `base` profile so every context that might invoke the shim
  (devShell, every wrapix container) has it available without per-user setup. macOS
  hosts running `git commit` outside `nix develop` are the only environment that can
  hit the missing-flock path, and the error message points them at the fix.
- **Install ordering** — `core.hooksPath` is set idempotently by the `flake.nix`
  devShell `shellHook` on every `nix develop` entry
  (`git config --local core.hooksPath lib/prek/hooks`). No per-user setup step is
  required, and the shims survive subsequent `prek install` runs because they live
  outside `.git/hooks/`. Contributors who commit outside `nix develop` and have never
  entered the devShell on this clone won't have `core.hooksPath` set and bypass FR2
  entirely — this is considered acceptable (rare, outside the concurrent-agent
  scenario); contributors who *have* entered the devShell but then commit outside it
  still have `core.hooksPath` set in their local git config and hit the *flock
  availability* bullet above instead (aborting loudly rather than silently racing).

## Affected Files

| File | Purpose |
|------|---------|
| `.pre-commit-config.yaml` | Hook stage configuration |
| `lib/prek/hooks/pre-commit` | `flock`-wrapped prek entry point for pre-commit stage |
| `lib/prek/hooks/pre-push` | `flock`-wrapped prek entry point for pre-push stage |
| `lib/prek/lock.sh` | Shared helper: resolve main-repo lock path, provide `_prek_acquire_lock` |
| `.gitignore` | Ignore `.wrapix/prek.lock`, `.wrapix/push-verified` |
| `flake.nix` devShell `shellHook` | Set `core.hooksPath = lib/prek/hooks` idempotently on entry |
| `flake.nix` devShell packages | Add `flock` (util-linux) |
| `lib/sandbox/profiles.nix` base profile | Add `flock` (util-linux) |

## Out of Scope

- Custom per-feature hook overrides — use a project-local `.pre-commit-config.yaml` patch
- Hook retry logic
- Parallel hook execution
