# Pre-commit Hooks

Staged git hooks via [prek](https://github.com/j178/prek), packaged as a Nix-store bundle so consumers don't vendor shims, run `prek install`, or manage `core.hooksPath` themselves.

## Problem Statement

A wrapix consumer using prek wants the same hook chain to fire in every context — host devshell, profile container, agent-driven bead clone — without per-context shim files, manual `prek install` steps, or `core.hooksPath` book-keeping. Wrapix ships one frozen Nix-store hook bundle and threads it through every install path so the consumer's `.pre-commit-config.yaml` is the only place hooks are configured.

## Architecture

All git hooks for prek-using wrapix repositories are served from a single Nix-store derivation — `wrapix.prekHooks` — pointed at by `core.hooksPath`. The bundle contains one shim per stage prek serves. Every shim invokes prek directly; the pre-commit and post-* shims `exec` it, while the pre-push shim wraps the call in a stamp-file dance to survive an SSH disconnect mid-check (see § Pre-Push Stamp Dance):

| Stage | Shim behavior |
|-------|---------------|
| pre-commit | `exec prek hook-impl --hook-type=pre-commit ...` |
| pre-push | `prek hook-impl --hook-type=pre-push ...` with `.wrapix/push-verified` stamp dance |
| prepare-commit-msg | `exec prek hook-impl --hook-type=prepare-commit-msg ...` |
| post-checkout | `exec prek hook-impl --hook-type=post-checkout ...` |
| post-merge | `exec prek hook-impl --hook-type=post-merge ...` |

Each shim uses `prek hook-impl --hook-type=<stage>` rather than `prek run` because git passes positional args that `prek run` would mistake for hook/project selectors.

`mkDevShell` (see `profiles.md` § Prek hook management) sets `core.hooksPath` to the bundle on every devshell entry whenever `.pre-commit-config.yaml` is present. Profile container images install the same bundle via the container entrypoint (see `image-builder.md` § Hook installation).

Consumers do not vendor shims, do not set `core.hooksPath` themselves, and do not run `prek install`. Git never reads `.git/hooks/` while `core.hooksPath` is set, so whatever lands there (e.g. `bd hooks install`) is inert — no chmod-lockdown is needed and no `prek install -f` runs from any wrapix lifecycle.

## Reference Hook Configuration

Wrapix's own `.pre-commit-config.yaml` declares the following stage→hook mapping. This is a reference example — downstream consumers configure their own hook list independently of what wrapix runs in its own tree:

| Stage | Hooks | Purpose |
|-------|-------|---------|
| pre-commit | treefmt, shellcheck, builtin hooks (trailing-whitespace, end-of-file-fixer, check-merge-conflict) | Fast validation on every commit |
| prepare-commit-msg | bd agent trailers | Add agent identity to commits |
| post-checkout | bd dolt pull | Pull Dolt state after branch switch |
| post-merge | bd dolt pull | Pull Dolt state after pull/merge |
| pre-push | nix flake check, loom integration tests | Slow validation before sharing |

## Pre-Push Stamp Dance

The pre-push shim runs prek's pre-push checks then writes the current HEAD SHA to `.wrapix/push-verified` and exits 0. If the SSH connection survived the check, git completes the push on it; otherwise the user re-runs `git push` and the stamp short-circuits on a fresh connection. This avoids SSH idle timeouts during long test suites.

The stamp is single-use, scoped to a specific HEAD SHA, and consumed on the next pre-push invocation that matches. It is not a long-lived approval token — a different HEAD or a stale-SHA mismatch invalidates it and the full check runs again.

## Hook-Entry Wrappers

`wrapix.prePushChecks` and `wrapix.skipIfMissing` are two sibling `writeShellScriptBin` derivations on `PATH` in both the host devShell and every profile container image. They sit alongside `wrapix.prekHooks` (separate surface: `PATH` rather than `core.hooksPath`) so that prek hook `entry:` lines can name them. One `.pre-commit-config.yaml` can then describe a single slow tier that runs across three contexts — host pre-push (git-driven), bead-container pre-push (git-driven), and any programmatic `prek run --hook-stage pre-push` invocation by an external driver — without per-context branching.

### `pre-push-checks`

Wraps a slow check with a marker-aware short-circuit. Contract:

```
pre-push-checks <command> [args…]
```

Resolution order:

1. If `.loom/marker.json` is absent in the current working directory, `exec "$@"`.
2. If `loom gate verify-marker` is missing from `PATH`, `exec "$@"`. Consumers without a loom-based driver loop install the wrapper safely; it transparently degrades to running the wrapped command.
3. Otherwise, invoke `loom gate verify-marker`:
   - exit 0 → wrapper exits 0 without running the wrapped command (short-circuit).
   - exit non-zero → `exec "$@"` (marker stale or invalid; fall through to the real check).

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

The `.wrapix/push-verified` stamp is a within-attempt retry safety net: the pre-push shim writes it automatically on success and consumes it only when a subsequent push retries the same HEAD after SSH death. `pre-push-checks` is a different layer — a per-entry opt-in skip when an external loom run has already validated the commit. Both can be active simultaneously. Long-term removal of `push-verified` is deferred until the marker mechanism covers the SSH-retry case too.

## Hook Installation in Profile Containers

Every profile container image installs the same prek setup that `mkDevShell` configures on the host: `core.hooksPath` resolves to `wrapix.prekHooks`, and both wrappers are on `PATH`. Agent commits and bead-branch pushes from inside the container therefore fire the same `.pre-commit-config.yaml` chain the host runs, with the same enforcement: a failing hook aborts the commit or push.

Profile container images do not ship `nix` by default. Nix-requiring hooks degrade via `skip-if-missing nix --` in the downstream `.pre-commit-config.yaml`, not via a wrapix-side hook-id skip list. Whether `nix` is on `PATH` is a property of the profile's packages; the wrapper makes hook firing conditional on it, so the same config is correct in both contexts.

See `image-builder.md` § Hook installation for the build-side mechanism (which PATH the wrappers land on, how the entrypoint sets `core.hooksPath`, and the platform-specific entrypoint paths).

## Success Criteria

- The `wrapix.prekHooks` derivation contains executable shims for `pre-commit`, `pre-push`, `prepare-commit-msg`, `post-checkout`, and `post-merge`
  [check](grep -nE '\$out/(pre-commit|pre-push|prepare-commit-msg|post-checkout|post-merge)' lib/prek/bundle.nix)
- `mkDevShell` sets `core.hooksPath` to the `wrapix.prekHooks` store path on every devshell entry when `.pre-commit-config.yaml` is present
  [check](grep -nE 'core\.hooksPath|prekHooks' lib/default.nix)
- The pre-commit and pre-push shims both invoke `prek hook-impl --hook-type=<stage>` (not `prek run`, which would mistake git's positional args for hook/project selectors)
  [check](grep -nE 'hook-impl --hook-type=' lib/prek/hooks/pre-commit lib/prek/hooks/pre-push)
- No shim sources `lock.sh`, calls `_prek_acquire_lock`, or invokes `flock`; every shim invokes `prek hook-impl --hook-type=<its-stage>`
  [system](bash tests/profiles/prek-hooks-bundle.sh test_shims_are_plain_hook_impl)
- The pre-push shim writes and consumes `.wrapix/push-verified`
  [check](grep -nE 'push-verified' lib/prek/hooks/pre-push)
- `wrapix.prePushChecks` and `wrapix.skipIfMissing` are exposed by the wrapix library and land on the host devShell's `PATH`
  [check](grep -nE 'prePushChecks|skipIfMissing' lib/default.nix)
- `pre-push-checks` exits 0 without running the wrapped command when `.loom/marker.json` is present and `loom gate verify-marker` exits 0
  [system](bash tests/prek/pre-push-checks-marker-valid.sh)
- `pre-push-checks` execs the wrapped command when `.loom/marker.json` is present and `loom gate verify-marker` exits non-zero
  [system](bash tests/prek/pre-push-checks-marker-stale.sh)
- `pre-push-checks` execs the wrapped command when `.loom/marker.json` is absent
  [system](bash tests/prek/pre-push-checks-no-marker.sh)
- `pre-push-checks` execs the wrapped command when `loom gate verify-marker` is not on `PATH`
  [system](bash tests/prek/pre-push-checks-no-loom.sh)
- `skip-if-missing <tool> -- <cmd>` execs `<cmd>` when `<tool>` resolves on `PATH`
  [system](bash tests/prek/skip-if-missing-present.sh)
- `skip-if-missing <tool> -- <cmd>` exits 0 without running `<cmd>` when `<tool>` is absent from `PATH`
  [system](bash tests/prek/skip-if-missing-absent.sh)
- A pre-commit hook configured in `.pre-commit-config.yaml` fires when `git commit` runs inside a profile container
  [system?](bash tests/sandbox/container-pre-commit.sh)
- A pre-push hook configured in `.pre-commit-config.yaml` fires when `git push` runs inside a profile container
  [system?](bash tests/sandbox/container-pre-push.sh)

## Requirements

### Functional

1. **Bundle ownership** — `wrapix.prekHooks` owns every staged hook shim; consumers do not vendor or override unless they substitute the whole bundle via `mkDevShell { prekHooks = <derivation>; }`.
2. **`core.hooksPath` management** — `mkDevShell` sets `core.hooksPath` idempotently per devshell entry; no per-user setup step.
3. **Hook stages** — pre-commit (treefmt, shellcheck, builtin hooks), prepare-commit-msg (bd trailers), post-checkout / post-merge (`bd dolt pull`), pre-push (`nix flake check`, loom integration tests).
4. **Stamp-file dance** — pre-push writes `.wrapix/push-verified` after a successful check so a subsequent push can short-circuit if the SSH connection died during validation. See § Pre-Push Stamp Dance for the full mechanic.
5. **Marker-aware short-circuit** — `pre-push-checks` consults `loom gate verify-marker`'s exit code to decide whether to skip the wrapped command; see § Hook-Entry Wrappers for the resolution order.
6. **Graceful degrade in wrappers** — `pre-push-checks` execs the wrapped command when either `loom gate verify-marker` or `.loom/marker.json` is absent. `skip-if-missing` exits 0 silently when `<tool>` is absent from `PATH`. Neither wrapper exits non-zero on a missing-input path.
7. **Container hook parity** — profile containers install `core.hooksPath` and both wrappers on `PATH` so `.pre-commit-config.yaml` fires equivalently inside the container and on the host.

### Non-Functional

1. **`--no-verify` honored** — git's standard hook-bypass flag is the documented escape hatch when a hook would otherwise block an emergency commit or push.
2. **Schema independence** — wrapix's coupling to loom is bounded by the file path `.loom/marker.json` and the `loom gate verify-marker` exit-code contract. Schema and validation evolution in loom does not require a wrapix change.
3. **No new runtime dependencies** — both wrappers are POSIX shell scripts using utilities already present in the devShell and profile-image base layer.

## Out of Scope

- Custom per-feature hook overrides — use a project-local `.pre-commit-config.yaml` patch
- Hook retry logic
- Parallel hook execution
- Parameterized `mkPrekHooks` constructor. v1 ships a single frozen `wrapix.prekHooks` bundle; consumers needing a different shim set substitute a hand-built derivation via `mkDevShell { prekHooks = <derivation>; }`. A parameterized constructor lands when a second concrete use case emerges.
- Cross-process serialization of prek hook execution. Earlier revisions wrapped every shim in a `flock`-guarded critical section to defend prek's stash/restore dance against two commits landing at once on the same working tree. That defense was retired because wrapix's consumer-invocation model already eliminates the dominant contention case: loom runs each bead in a private `git clone --local` under `.loom/beads/<id>/` with a single sequential agent per container and never touches the operator's `/workspace/`, so no concurrent writers reach a shared working tree under loom's control. Residual cases (parallel host shells on a single working tree, non-loom linked worktrees) use `git commit --no-verify` / `git push --no-verify` as the escape hatch.
- `marker.json` schema, mint, and validation — owned by downstream loom.
- `loom gate verify-marker` subcommand internals — owned by downstream loom.
- `.pre-commit-config.yaml` content in downstream projects — wrapix ships the wrappers, not the hook list.
- `push-verified` stamp deprecation/removal — orthogonal to `pre-push-checks` (see § Relationship to `push-verified`); deferred until the marker mechanism covers the SSH-retry case.
- A wrapix-owned hook-id skip list for the bead container — nix absence is handled via `skip-if-missing` at the point of use, not via a wrapix-side filter.
