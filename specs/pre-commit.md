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

All git hooks for prek-using wrapix repositories are served from a single
Nix-store directory — `wrapix.prekHooks` — pointed at by `core.hooksPath`. The
bundle contains shims for every stage prek serves:

| Stage | Shim behavior |
|-------|---------------|
| pre-commit | flock-wrap then `exec prek hook-impl --hook-type=pre-commit ...` |
| pre-push | flock-wrap then `prek run --stage pre-push` with stamp-file dance |
| prepare-commit-msg | plain `exec prek hook-impl ...` (no flock) |
| post-checkout | plain `exec prek hook-impl ...` (no flock) |
| post-merge | plain `exec prek hook-impl ...` (no flock) |

`mkDevShell` sets `core.hooksPath` to the bundle on every devshell entry
whenever `.pre-commit-config.yaml` is present (see `specs/profiles.md`
§ Prek hook management for the auto-set / opt-out / derivation-substitute
contract). Consumers do not vendor shims, do not set `core.hooksPath`
themselves, and do not run `prek install`.

Because git never reads `.git/hooks/` while `core.hooksPath` is set,
whatever lands there (e.g. `bd hooks install`) is inert — no chmod-lockdown
is needed, no `prek install -f` runs from any wrapix lifecycle. Beads local
hooks declared in `.pre-commit-config.yaml` still run via prek's own
dispatch, unaffected by the location swap.

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

- **Mechanism** — the `pre-commit` and `pre-push` shims inside the
  `wrapix.prekHooks` derivation source `_lib/lock.sh` for shared lock
  infrastructure, then call `_prek_acquire_lock` to serialize. The pre-commit
  shim `exec`s `prek hook-impl --hook-dir ... --script-version 4
  --hook-type=pre-commit -- "$@"` (`hook-impl` is used rather than `prek run`
  because git passes positional args that `prek run` would mistake for
  hook/project selectors). The pre-push shim runs `prek run --stage pre-push`
  independently of git's SSH connection, then writes a stamp file
  (`.wrapix/push-verified`) on success; the user re-runs `git push` and the
  stamp is consumed instantly, pushing on a fresh connection. This avoids SSH
  idle timeouts during long test suites. Git is pointed at the bundle directory
  via `core.hooksPath = ${wrapix.prekHooks}`, set by `mkDevShell`.
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
- **Install ordering** — `core.hooksPath` is set idempotently by `mkDevShell`'s
  lifecycle on every devshell entry, pointing at the `wrapix.prekHooks`
  Nix-store path (see `specs/profiles.md` § Prek hook management). No per-user
  setup step is required, and the shims survive arbitrary `prek install` runs
  because they live entirely outside `.git/hooks/` — no wrapix lifecycle calls
  `prek install` at all.
- **Contributors outside `nix develop`** — those who have never entered the
  devshell on this clone won't have `core.hooksPath` set and bypass FR2
  entirely. Once any devshell entry has run, `core.hooksPath` is persisted in
  local git config and every later commit hits the shim regardless of whether
  the commit fires from inside `nix develop`. Contributors who have entered
  the devshell but commit outside it still hit the *flock availability* bullet
  above (aborting loudly rather than silently racing).

## Affected Files

| File | Purpose |
|------|---------|
| `.pre-commit-config.yaml` | Hook stage configuration |
| `lib/prek/hooks/pre-commit` | `flock`-wrapped shim — source input to `wrapix.prekHooks` derivation |
| `lib/prek/hooks/pre-push` | `flock`-wrapped shim — source input to `wrapix.prekHooks` derivation |
| `lib/prek/hooks/prepare-commit-msg` | Plain prek shim — source input to `wrapix.prekHooks` derivation |
| `lib/prek/hooks/post-checkout` | Plain prek shim — source input to `wrapix.prekHooks` derivation |
| `lib/prek/hooks/post-merge` | Plain prek shim — source input to `wrapix.prekHooks` derivation |
| `lib/prek/lock.sh` | Shared helper — source input to `wrapix.prekHooks`; resolves main-repo lock path, provides `_prek_acquire_lock` |
| `lib/default.nix` | `wrapix.prekHooks` derivation; `mkDevShell` lifecycle sets `core.hooksPath` |
| `.gitignore` | Ignore `.wrapix/prek.lock`, `.wrapix/push-verified` |
| `flake.nix` devShell packages | Add `flock` (util-linux) |
| `lib/sandbox/profiles.nix` base profile | Add `flock` (util-linux) |

## Out of Scope

- Custom per-feature hook overrides — use a project-local `.pre-commit-config.yaml` patch
- Hook retry logic
- Parallel hook execution
- Parameterized `mkPrekHooks` constructor. v1 ships a single frozen
  `wrapix.prekHooks` bundle; consumers needing a different shim set substitute
  a hand-built derivation via `mkDevShell { prekHooks = <derivation>; }`. A
  parameterized constructor lands when a second concrete use case emerges and
  the right configuration surface is clearer than guessing now.
- Flock serialization on `prepare-commit-msg`, `post-checkout`, `post-merge`.
  These stages don't enter prek's stash window (wx-m5yuq is specifically about
  the working-tree stash on commit/push), so flock would only serialize
  unrelated operations across agents. If a future bug shows a different race
  on one of these stages, the bundle gains a shim for it — additive change,
  no contract break.
