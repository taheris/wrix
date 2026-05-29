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

## Hook-Entry Wrappers

`wrapix.prePushChecks` and `wrapix.skipIfMissing` are two sibling `writeShellScriptBin` derivations on `PATH` in both the host devShell and every profile container image. They sit alongside `wrapix.prekHooks` (separate surface: `PATH` rather than `core.hooksPath`) so that prek hook `entry:` lines can name them. One `.pre-commit-config.yaml` can then describe a single slow tier that runs across three contexts — host pre-push (git-driven), bead-container pre-push (git-driven), and any programmatic `prek run --hook-stage pre-push` invocation by an external driver — without per-context branching.

### `pre-push-checks`

Wraps a slow check with a marker-aware short-circuit. Contract:

```
pre-push-checks <command> [args…]
```

Resolution order:

1. If `.wrapix/loom/marker.json` is absent in the current working directory, `exec "$@"`.
2. If present, invoke `loom gate verify-marker`:
   - exit 0 → wrapper exits 0 without running the wrapped command (short-circuit).
   - exit non-zero → `exec "$@"` (marker stale or invalid; fall through to the real check).
3. If `loom gate verify-marker` is missing from `PATH`, `exec "$@"`. Consumers without a loom-based driver loop install the wrapper safely; it transparently degrades to running the wrapped command.

The wrapper does **not** read or interpret `marker.json`. Schema, mint, and validation are owned by the downstream loom project; wrapix's only responsibility is "ask `loom gate verify-marker`; act on the exit code." Schema evolution in loom does not propagate to wrapix.

Downstream `.pre-commit-config.yaml` references the wrapper by name:

```yaml
- id: cargo-clippy
  entry: pre-push-checks cargo clippy --workspace --all-targets -- -D warnings
  language: system
  stages: [pre-push]
  files: \.rs$
```

Downstream positions `loom gate verify-marker` as the first hook in the slow tier, wired directly (`entry: loom gate verify-marker`, no wrapper). Routing the canonical check through `pre-push-checks` would be self-referential — the wrapper consults `verify-marker` to decide whether to skip; the marker check itself cannot skip itself.

### `skip-if-missing`

Wraps a check whose required tool may not be on `PATH` in every context. Contract:

```
skip-if-missing <tool> -- <command> [args…]
```

If `<tool>` resolves on `PATH`, `exec` the command. If absent, exit 0 silently. The primary use is making nix-requiring hooks inert inside the bead container, where `nix` is intentionally absent:

```yaml
- id: nix-flake-check
  entry: skip-if-missing nix -- nix flake check
  language: system
  stages: [pre-push]
```

The same wrapper generalizes to any runtime dep (`cargo`, `docker`, …) downstream needs to tag at the point of use. Knowledge of "this hook needs `<tool>`" lives in the hook entry, next to the command — not in a wrapix-curated skip list.

### Relationship to `push-verified`

The existing `.wrapix/push-verified` SHA stamp written by the pre-push shim remains the path for projects that have not adopted the marker. The two coexist during transition: a project pre-marker uses `push-verified`; an adopter uses `pre-push-checks`. No data migration — the formats differ (single-line SHA vs. loom-validated JSON marker), and a project using one does not have the other. Long-term removal of `push-verified` is deferred until adopters have migrated.

## Bead-Container Hook Installation

Every profile container image installs the same prek setup that `mkDevShell` configures on the host: `core.hooksPath` resolves to `wrapix.prekHooks`, and both wrappers are on `PATH`. Agent commits and bead-branch pushes from inside the container therefore fire the same `.pre-commit-config.yaml` chain the host runs, with the same enforcement: a failing hook aborts the commit or push.

Profile container images do not ship `nix` by default. Nix-requiring hooks degrade via `skip-if-missing nix --` in the downstream `.pre-commit-config.yaml`, not via a wrapix-side hook-id skip list. Whether `nix` is on `PATH` is a property of the profile's packages; the wrapper makes hook firing conditional on it, so the same config is correct in both contexts.

See `image-builder.md` § Hook installation for the build-side mechanism (which PATH the wrappers land on, how the entrypoint sets `core.hooksPath`, and the platform-specific entrypoint paths).

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
- `wrapix.prePushChecks` and `wrapix.skipIfMissing` are exposed by the wrapix library and land on the host devShell's `PATH`
  [check?](grep -nE 'prePushChecks|skipIfMissing' lib/default.nix)
- `pre-push-checks` exits 0 without running the wrapped command when `.wrapix/loom/marker.json` is present and `loom gate verify-marker` exits 0
  [system?](bash tests/prek/pre-push-checks-marker-valid.sh)
- `pre-push-checks` execs the wrapped command when `.wrapix/loom/marker.json` is present and `loom gate verify-marker` exits non-zero
  [system?](bash tests/prek/pre-push-checks-marker-stale.sh)
- `pre-push-checks` execs the wrapped command when `.wrapix/loom/marker.json` is absent
  [system?](bash tests/prek/pre-push-checks-no-marker.sh)
- `pre-push-checks` execs the wrapped command when `loom gate verify-marker` is not on `PATH`
  [system?](bash tests/prek/pre-push-checks-no-loom.sh)
- `skip-if-missing <tool> -- <cmd>` execs `<cmd>` when `<tool>` resolves on `PATH`
  [system?](bash tests/prek/skip-if-missing-present.sh)
- `skip-if-missing <tool> -- <cmd>` exits 0 without running `<cmd>` when `<tool>` is absent from `PATH`
  [system?](bash tests/prek/skip-if-missing-absent.sh)
- A pre-commit hook configured in `.pre-commit-config.yaml` fires when `git commit` runs inside a profile container
  [system?](bash tests/sandbox/container-pre-commit.sh)
- A pre-push hook configured in `.pre-commit-config.yaml` fires when `git push` runs inside a profile container
  [system?](bash tests/sandbox/container-pre-push.sh)

## Requirements

### Functional

1. **Bundle ownership** — `wrapix.prekHooks` owns every staged hook shim; consumers do not vendor or override unless they substitute the whole bundle via `mkDevShell { prekHooks = <derivation>; }`.
2. **`core.hooksPath` management** — `mkDevShell` sets `core.hooksPath` idempotently per devshell entry; no per-user setup step.
3. **Hook stages** — pre-commit (treefmt, shellcheck, builtin hooks), prepare-commit-msg (bd trailers), post-checkout / post-merge (`bd dolt pull`), pre-push (`nix flake check`, loom integration tests).
4. **Flock serialization** — pre-commit and pre-push acquire `.wrapix/prek.lock` exclusively before invoking prek; lock path resolves via `git rev-parse --git-common-dir` so worktrees share the main repo's lock.
5. **Stamp-file dance** — pre-push runs validation off the SSH connection, stamps `.wrapix/push-verified` on success, and the user re-runs `git push` to consume the stamp.
6. **Loud failure** — missing `flock(1)` or 600s lock timeout aborts with stderr context and non-zero exit; no silent bypass, no infinite wait.
7. **Marker-aware short-circuit** — `pre-push-checks` consults `loom gate verify-marker`'s exit code to decide whether to skip the wrapped command; see § Hook-Entry Wrappers for the resolution order.
8. **Graceful degrade in wrappers** — `pre-push-checks` execs the wrapped command when either `loom gate verify-marker` or `.wrapix/loom/marker.json` is absent. `skip-if-missing` exits 0 silently when `<tool>` is absent from `PATH`. Neither wrapper exits non-zero on a missing-input path.
9. **Container hook parity** — profile containers install `core.hooksPath` and both wrappers on `PATH` so `.pre-commit-config.yaml` fires equivalently inside the bead container and on the host.

### Non-Functional

1. **Cross-process serialization** — pre-commit and pre-push concurrent calls across host shells, containers, and linked worktrees all coordinate through the same lock file.
2. **No FD inheritance** — child processes do not retain the lock FD (`9>&-`), so orphaned subprocesses cannot extend the critical section.
3. **`--no-verify` honored** — the documented escape hatch skips both the hook and prek's stash, so no race exists on that path.
4. **Schema independence** — wrapix's coupling to loom is bounded by the file path `.wrapix/loom/marker.json` and the `loom gate verify-marker` exit-code contract. Schema and validation evolution in loom does not require a wrapix change.
5. **No new runtime dependencies** — both wrappers are POSIX shell scripts using utilities already present in the devShell and profile-image base layer.

## Out of Scope

- Custom per-feature hook overrides — use a project-local `.pre-commit-config.yaml` patch
- Hook retry logic
- Parallel hook execution
- Parameterized `mkPrekHooks` constructor. v1 ships a single frozen `wrapix.prekHooks` bundle; consumers needing a different shim set substitute a hand-built derivation via `mkDevShell { prekHooks = <derivation>; }`. A parameterized constructor lands when a second concrete use case emerges.
- Flock serialization on `prepare-commit-msg`, `post-checkout`, `post-merge`. These stages don't enter prek's stash window, so flock would only serialize unrelated operations across agents.
- `marker.json` schema, mint, and validation — owned by downstream loom.
- `loom gate verify-marker` subcommand internals — owned by downstream loom.
- `.pre-commit-config.yaml` content in downstream projects — wrapix ships the wrappers, not the hook list.
- `push-verified` stamp deprecation/removal — coexists with `pre-push-checks` during transition; removal is deferred until adopters have migrated.
- A wrapix-owned hook-id skip list for the bead container — nix absence is handled via `skip-if-missing` at the point of use, not via a wrapix-side filter.
