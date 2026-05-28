# Pre-commit Hooks

Staged git hooks via [prek](https://github.com/j178/prek) with a `flock`-serialized shim to prevent concurrent stash races across agents sharing a workspace.

## Problem Statement

prek's stash/restore dance around unstaged changes races with concurrent writers, silently dropping working-tree edits (wx-m5yuq). Multiple wrapix agents on the same workspace — and `loom run` quality-gate commits firing alongside human commits — re-expose this race unless hook invocations serialize across processes.

## Architecture

All git hooks for prek-using wrapix repositories are served from a single Nix-store derivation — `wrapix.prekHooks` — pointed at by `core.hooksPath`. The bundle contains a shim for every stage prek serves. Two shims (pre-commit, pre-push) wrap an `flock`-serialized critical section; the others execute prek directly:

| Stage | Shim behavior |
|-------|---------------|
| pre-commit | flock-wrap then `exec prek hook-impl --hook-type=pre-commit ...` |
| pre-push | flock-wrap then `prek run --stage pre-push` with stamp-file dance |
| prepare-commit-msg | plain `exec prek hook-impl ...` (no flock) |
| post-checkout | plain `exec prek hook-impl ...` (no flock) |
| post-merge | plain `exec prek hook-impl ...` (no flock) |

`mkDevShell` (see `profiles.md` § Prek hook management) sets `core.hooksPath` to the bundle on every devshell entry whenever `.pre-commit-config.yaml` is present. Consumers do not vendor shims, do not set `core.hooksPath` themselves, and do not run `prek install`. Git never reads `.git/hooks/` while `core.hooksPath` is set, so whatever lands there (e.g. `bd hooks install`) is inert — no chmod-lockdown is needed and no `prek install -f` runs from any wrapix lifecycle.

Beads local hooks declared in `.pre-commit-config.yaml` still run via prek's own dispatch, unaffected by the location swap.

## Hook Stage Configuration

`.pre-commit-config.yaml` declares the staged hooks the bundle dispatches:

| Stage | Hooks | Purpose |
|-------|-------|---------|
| pre-commit | treefmt, shellcheck, builtin hooks (trailing-whitespace, end-of-file-fixer, check-merge-conflict) | Fast validation on every commit |
| prepare-commit-msg | bd agent trailers | Add agent identity to commits |
| post-checkout | bd dolt pull | Pull Dolt state after branch switch |
| post-merge | bd dolt pull | Pull Dolt state after pull/merge |
| pre-push | nix flake check, loom integration tests | Slow validation before sharing |

## Flock Serialization Contract

The pre-commit and pre-push shims acquire an exclusive `flock` on `.wrapix/prek.lock` before invoking prek's hook run and release it when prek exits.

- **Mechanism** — the shims source `_lib/lock.sh` from inside `wrapix.prekHooks` and call `_prek_acquire_lock` to serialize. The pre-commit shim `exec`s `prek hook-impl --hook-dir ... --script-version 4 --hook-type=pre-commit -- "$@"` (`hook-impl` rather than `prek run` because git passes positional args that `prek run` would mistake for hook/project selectors). The pre-push shim runs `prek run --stage pre-push` independently of git's SSH connection, then writes a stamp file (`.wrapix/push-verified`) on success; the user re-runs `git push` and the stamp is consumed instantly, pushing on a fresh connection. This avoids SSH idle timeouts during long test suites.
- **Lock file** — `.wrapix/prek.lock`, gitignored, auto-created on first use. Located in the main repo's `.wrapix/` so every agent (host, container with `.wrapix` mount, linked worktree) shares the same lock. A single lock covers both stages because both protect the same working-tree stash window.
- **Worktree-safe path resolution** — the shim resolves the lock path via `$(dirname "$(git rev-parse --git-common-dir)")/.wrapix/prek.lock`. `git rev-parse --git-common-dir` always returns the main repo's `.git/` from any linked worktree, so commits fired from a linked worktree share the same lock as the main repo.
- **Timeout** — 600s poll loop with dead-PID recovery. If the lock holder's PID is no longer running, the lock file is deleted and re-acquired on a fresh inode. If the timeout fires, the shim fails loudly (`>&2` with lock-holder PID) and exits non-zero so the commit or push aborts rather than deadlocking the workspace.
- **No FD inheritance** — subprocesses (prek, nix) do not inherit the lock FD (`9>&-`) in the pre-push shim, preventing orphaned children from holding stale locks.
- **`--no-verify` bypass** — skips the hook and therefore the lock, but also skips prek's stash, so no race exists on that path. The documented escape hatch for emergency pushes.
- **`flock` availability** — `flock(1)` is a hard requirement, not a graceful-degrade. The shim aborts with a clear error (`flock not found — enter nix develop or install util-linux`) if `flock` is missing from `PATH`. `flock` is added to the nix devShell and to the wrapix sandbox `base` profile so every context that might invoke the shim has it available. macOS hosts running `git commit` outside `nix develop` are the only environment that can hit the missing-flock path.

## Success Criteria

- The `wrapix.prekHooks` derivation contains executable shims for `pre-commit`, `pre-push`, `prepare-commit-msg`, `post-checkout`, and `post-merge`
  [check](p=$(nix build --no-link --print-out-paths .#prekHooks) && test -x "$p/pre-commit" -a -x "$p/pre-push" -a -x "$p/prepare-commit-msg" -a -x "$p/post-checkout" -a -x "$p/post-merge")
- `mkDevShell` sets `core.hooksPath` to the `wrapix.prekHooks` store path on every devshell entry when `.pre-commit-config.yaml` is present
  [check](grep -nE 'core\.hooksPath|prekHooks' lib/default.nix)
- The pre-commit and pre-push shims source `_lib/lock.sh` and call `_prek_acquire_lock` before invoking prek
  [check](grep -nE '_prek_acquire_lock|lock\.sh' lib/prek/hooks/pre-commit lib/prek/hooks/pre-push)
- The pre-commit shim invokes `prek hook-impl --hook-type=pre-commit` (not `prek run`), and the pre-push shim invokes `prek run --stage pre-push`
  [check](grep -nE 'hook-impl|prek run' lib/prek/hooks/pre-commit lib/prek/hooks/pre-push)
- Lock-file path resolves to `$(git rev-parse --git-common-dir)/../.wrapix/prek.lock` so linked worktrees share the main repo's lock
  [check](grep -q 'git-common-dir' lib/prek/lock.sh && grep -q 'prek\.lock' lib/prek/lock.sh)
- Lock FD is closed (`9>&-`) before exec'ing subprocesses in the pre-push shim
  [check](grep -nE '9>&-' lib/prek/hooks/pre-push)
- Missing `flock(1)` aborts the shim with a clear stderr message and non-zero exit, rather than silently bypassing
  [check](grep -nE 'flock not found|flock.*PATH' lib/prek/lock.sh)
- `flock` (util-linux) is present in both the nix devShell and the wrapix `base` profile
  [check](grep -nE 'util-linux|flock' flake.nix lib/sandbox/profiles.nix)
- The pre-push shim writes `.wrapix/push-verified` on success; a subsequent `git push` consumes it on a fresh connection
  [check](grep -nE 'push-verified' lib/prek/hooks/pre-push)
- Two concurrent pre-commit hooks against the same workspace serialize (the second blocks until the first releases) without losing working-tree edits
  [system](bash tests/prek/concurrent-pre-commit.sh)
- A commit fired from a linked git worktree acquires the main repo's lock rather than a sibling lock under the worktree dir
  [system](bash tests/prek/worktree-lock-resolution.sh)

## Requirements

### Functional

1. **Bundle ownership** — `wrapix.prekHooks` owns every staged hook shim; consumers do not vendor or override unless they substitute the whole bundle via `mkDevShell { prekHooks = <derivation>; }`.
2. **`core.hooksPath` management** — `mkDevShell` sets `core.hooksPath` idempotently per devshell entry; no per-user setup step.
3. **Hook stages** — pre-commit (treefmt, shellcheck, builtin hooks), prepare-commit-msg (bd trailers), post-checkout / post-merge (`bd dolt pull`), pre-push (`nix flake check`, loom integration tests).
4. **Flock serialization** — pre-commit and pre-push acquire `.wrapix/prek.lock` exclusively before invoking prek; lock path resolves via `git rev-parse --git-common-dir` so worktrees share the main repo's lock.
5. **Stamp-file dance** — pre-push runs validation off the SSH connection, stamps `.wrapix/push-verified` on success, and the user re-runs `git push` to consume the stamp.
6. **Loud failure** — missing `flock(1)` or 600s lock timeout aborts with stderr context and non-zero exit; no silent bypass, no infinite wait.

### Non-Functional

1. **Cross-process serialization** — pre-commit and pre-push concurrent calls across host shells, containers, and linked worktrees all coordinate through the same lock file.
2. **No FD inheritance** — child processes do not retain the lock FD (`9>&-`), so orphaned subprocesses cannot extend the critical section.
3. **`--no-verify` honored** — the documented escape hatch skips both the hook and prek's stash, so no race exists on that path.

## Out of Scope

- Custom per-feature hook overrides — use a project-local `.pre-commit-config.yaml` patch
- Hook retry logic
- Parallel hook execution
- Parameterized `mkPrekHooks` constructor. v1 ships a single frozen `wrapix.prekHooks` bundle; consumers needing a different shim set substitute a hand-built derivation via `mkDevShell { prekHooks = <derivation>; }`. A parameterized constructor lands when a second concrete use case emerges.
- Flock serialization on `prepare-commit-msg`, `post-checkout`, `post-merge`. These stages don't enter prek's stash window, so flock would only serialize unrelated operations across agents.
