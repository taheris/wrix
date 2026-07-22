# Pre-commit Hooks

Staged git hooks via [prek](https://github.com/j178/prek), packaged as a Nix-store bundle so consumers don't vendor shims, run `prek install`, or manage `core.hooksPath` themselves.

## Problem Statement

A wrix consumer using prek wants the same hook chain to fire in every context — host devshell, initialized host checkout, profile container, Loom driver worktree, and agent-driven bead clone — without per-context shim files, manual `prek install` steps, or consumer-owned `core.hooksPath` book-keeping. Wrix ships one frozen Nix-store hook bundle and threads it through Wrix-owned install paths so the consumer's `.pre-commit-config.yaml` is the only place hooks are configured.

## Architecture

All git hooks for prek-using wrix repositories are served from a single Nix-store derivation — `wrix.prekHooks` — pointed at by `core.hooksPath`. The bundle contains one shim per stage prek serves. Every shim injects the Nix-store `prek` package into `PATH` before invoking prek, so hooks work from plain host shells, devshells, and profile containers. The pre-commit and post-* shims `exec` it, while the pre-push shim wraps the call in a stamp-file dance to survive an SSH disconnect mid-check (see § Pre-Push Stamp Dance):

| Stage | Shim behavior |
|-------|---------------|
| pre-commit | `exec prek hook-impl --hook-type=pre-commit ...` |
| pre-push | `prek hook-impl --hook-type=pre-push ...` with `.wrix/push-verified` stamp dance |
| prepare-commit-msg | `exec prek hook-impl --hook-type=prepare-commit-msg ...` |
| post-checkout | `exec prek hook-impl --hook-type=post-checkout ...` |
| post-merge | `exec prek hook-impl --hook-type=post-merge ...` |

Each shim uses `prek hook-impl --hook-type=<stage>` rather than `prek run` because git passes positional args that `prek run` would mistake for hook/project selectors.

`mkDevShell` (see `profiles.md` § Prek hook management) sets `core.hooksPath` to the bundle on every devshell entry whenever `.pre-commit-config.yaml` is present. `wrix init` (see `cli.md`) applies the same bundle for ordinary host Git and Loom driver worktrees outside the devshell. Profile container images install the same bundle via the container entrypoint (see `image-builder.md` § Hook installation).

Consumers do not vendor shims, do not set `core.hooksPath` themselves, and do not run `prek install`. Git never reads `.git/hooks/` while `core.hooksPath` is set, so whatever lands there (e.g. `bd hooks install`) is inert — no chmod-lockdown is needed and no `prek install -f` runs from any wrix lifecycle.

## Reference Hook Configuration

Wrix's own `.pre-commit-config.yaml` declares only the hook stages Wrix runs in this repository. This mapping is intentionally narrower than the shim bundle above; downstream consumers configure their own hook list independently of what Wrix runs in its own tree.

| Stage | Hooks | Purpose |
|-------|-------|---------|
| pre-commit | treefmt, shell re-exec guard, builtin hooks (trailing-whitespace, end-of-file-fixer, check-merge-conflict) | Fast validation on every commit |
| pre-push | nix flake check, loom gate verify | Slow validation before sharing; `test-ci:` verifier targets run on Linux and skip by default on Darwin |

The bundle still exposes `prepare-commit-msg`, `post-checkout`, and `post-merge` shims. Projects that want bd agent trailers or automatic `bd dolt pull` can add those hooks to their own `.pre-commit-config.yaml` or use bd-owned installation guidance; those bd hooks are examples, not Wrix's repository contract.

## Pre-Push Stamp Dance

The pre-push shim runs prek's pre-push checks then writes the current HEAD SHA to `.wrix/push-verified` and exits 0. If the SSH connection survived the check, git completes the push on it; otherwise the user re-runs `git push` and the stamp short-circuits on a fresh connection. This avoids SSH idle timeouts during long test suites.

The stamp is single-use, scoped to a specific HEAD SHA, and consumed on the next pre-push invocation that matches. It is not a long-lived approval token — a different HEAD or a stale-SHA mismatch invalidates it and the full check runs again.

## Hook-Entry Wrappers

`wrix.prePushChecks` and `wrix.skipIfMissing` are two sibling `writeShellScriptBin` derivations on `PATH` in both the host devShell and every profile container image. They sit alongside `wrix.prekHooks` (separate surface: `PATH` rather than `core.hooksPath`) so that prek hook `entry:` lines can name them. The `prePushChecks` derivation packages the same script that this repository exposes at `bin/pre-push-checks`. Loom-managed repositories invoke the repo-local path so Loom can parse stable per-hook marker metadata without relying on ambient `PATH`; other consumers can use the packaged command. One `.pre-commit-config.yaml` can then describe a single slow tier that runs across host pre-push, bead-container pre-push, and programmatic `prek run --hook-stage pre-push` invocations without per-context branching.

### `pre-push-checks`

Wraps a slow check with a marker-aware, per-hook short-circuit. Contract:

```
bin/pre-push-checks --hook-id <id> [--hook-entry <entry>] [--push-range <range>] -- <command> [args…]
```

Resolution order:

1. If `.loom/marker.json` is absent in the current working directory, execute the wrapped command.
2. If a hook id is present and entry metadata is omitted, derive the stable entry identity from the wrapped command. If the hook id is absent, execute the wrapped command without a marker shortcut.
3. Resolve an omitted push range from the current upstream, falling back to the literal `@{u}..HEAD` range when no upstream is configured.
4. If `loom` is missing from `PATH`, execute the wrapped command.
5. Otherwise, invoke `loom gate verify-marker` with the hook id, hook entry, and push range:
   - exit 0 → wrapper exits 0 without running the wrapped command.
   - exit non-zero → execute the wrapped command.

The wrapper does **not** read or interpret `marker.json`. Schema, mint, and validation are owned by the downstream Loom project; Wrix supplies the hook identity, entry, and range to `loom gate verify-marker` and acts on its exit code.

Loom-managed `.pre-commit-config.yaml` entries use folded YAML for readability; the wrapper derives marker metadata from the wrapped command:

```yaml
- id: cargo-clippy
  entry: >-
    bin/pre-push-checks --hook-id cargo-clippy --
    cargo clippy --workspace --all-targets -- -D warnings
  language: system
  stages: [pre-push]
  files: \.rs$
```

`loom gate verify-marker` is not a standalone pre-push hook. Each slow hook asks the wrapper whether a marker covers that exact hook and falls through normally when it does not.

### `skip-if-missing`

Wraps a check whose required tool may not be on `PATH` in every context. Contract:

```
skip-if-missing <tool> -- <command> [args…]
```

If `<tool>` resolves on `PATH`, `exec` the command. If absent, exit 0 silently. The primary use is keeping hooks inert in downstream profiles that explicitly omit a runtime dependency, or outside-Wrix contexts where the dependency is optional:

```yaml
- id: custom-nix-check
  entry: skip-if-missing nix -- nix flake check
  language: system
  stages: [pre-push]
```

The same wrapper generalizes to any runtime dep (`cargo`, `docker`, …) downstream needs to tag at the point of use. Knowledge of "this hook needs `<tool>`" lives in the hook entry, next to the command — not in a wrix-curated skip list.

### Relationship to `push-verified`

The `.wrix/push-verified` stamp is a within-attempt retry safety net: the pre-push shim writes it automatically on success and consumes it only when a subsequent push retries the same HEAD after SSH death. `pre-push-checks` is a different layer — a per-entry opt-in skip when an external loom run has already validated the commit. Both can be active simultaneously. Long-term removal of `push-verified` is deferred until the marker mechanism covers the SSH-retry case too.

## Hook Installation in Profile Containers

Every profile container image installs the same prek setup that `mkDevShell` configures on the host: `core.hooksPath` resolves to `wrix.prekHooks`, and both wrappers are on `PATH`. Agent commits and bead-branch pushes from inside the container therefore fire the same `.pre-commit-config.yaml` chain the host runs, with the same enforcement: a failing hook aborts the commit or push.

Wrix's own pre-push contract requires `nix flake check`, so a profile running this repository's hooks must provide `nix`; absence is a hook failure. Downstream repositories may still mark their own genuinely optional tools with `skip-if-missing` at the point of use rather than relying on a wrix-side hook-id skip list.

See `image-builder.md` § Hook installation for the build-side mechanism (which PATH the wrappers land on, how the entrypoint sets `core.hooksPath`, and the platform-specific entrypoint paths).

## Success Criteria

- The `wrix.prekHooks` derivation contains executable shims for `pre-commit`, `pre-push`, `prepare-commit-msg`, `post-checkout`, and `post-merge`
  [check](verify:prek.bundle-contents)
- `mkDevShell` sets `core.hooksPath` to the `wrix.prekHooks` store path on every devshell entry when `.pre-commit-config.yaml` is present
  [system](verify:prek.devshell-auto-set)
- The pre-commit and pre-push shims both invoke `prek hook-impl --hook-type=<stage>` (not `prek run`, which would mistake git's positional args for hook/project selectors)
  [system](verify:prek.shims-use-hook-impl)
- No shim sources `lock.sh`, calls `_prek_acquire_lock`, or invokes `flock`; every shim invokes `prek hook-impl --hook-type=<its-stage>` and pins the Nix-store `prek` package on `PATH`
  [system](verify:prek.shims-no-flock)
- The pre-push shim writes and consumes `.wrix/push-verified`
  [system](verify:prek.pre-push-stamp)
- `wrix.prePushChecks` and `wrix.skipIfMissing` are exposed by the wrix library and land on the host devShell's `PATH`
  [check](verify:prek.wrappers-on-devshell-path)
- Wrix's own `.pre-commit-config.yaml` matches the § Reference Hook Configuration stage→hook mapping, with hooks for `pre-commit` and `pre-push` only and no `prepare-commit-msg`, `post-checkout`, or `post-merge` hook entries
  [check](verify:prek.config-stage-set)
- Every Wrix pre-push entry uses `bin/pre-push-checks`, passes its own id, derives exact entry metadata from the wrapped command, and separates wrapper arguments from the command
  [system](verify:prek.config-wrapper-contract)
- `pre-push-checks` passes the hook id, entry, and push range to `loom gate verify-marker` and exits 0 without running the wrapped command when marker validation succeeds
  [system](verify:prek.pre-push-checks-marker-valid)
- `pre-push-checks` execs the wrapped command when `.loom/marker.json` is present and `loom gate verify-marker` exits non-zero
  [system](verify:prek.pre-push-checks-marker-stale)
- `pre-push-checks` execs the wrapped command when `.loom/marker.json` is absent
  [system](verify:prek.pre-push-checks-no-marker)
- `pre-push-checks` derives omitted entry metadata from the wrapped command and execs without consulting Loom when the hook id is absent
  [system](verify:prek.pre-push-checks-no-metadata)
- `pre-push-checks` execs the wrapped command when `loom gate verify-marker` is not on `PATH`
  [system](verify:prek.pre-push-checks-no-loom)
- `skip-if-missing <tool> -- <cmd>` execs `<cmd>` when `<tool>` resolves on `PATH`
  [system](verify:prek.skip-if-missing-present)
- `skip-if-missing <tool> -- <cmd>` exits 0 without running `<cmd>` when `<tool>` is absent from `PATH`
  [system](verify:prek.skip-if-missing-absent)
- Wrix's own `nix-flake-check` hook is the first pre-push entry, requires `nix` to be present, and wraps `nix flake check` with canonical derived marker metadata
  [system](verify:prek.config-wrapper-contract)
- A pre-commit hook configured in `.pre-commit-config.yaml` fires when `git commit` runs inside a profile container
  [system](verify:prek.container-pre-commit)
- A pre-push hook configured in `.pre-commit-config.yaml` fires when `git push` runs inside a profile container
  [system](verify:prek.container-pre-push)
- `git commit --no-verify` and `git push --no-verify` bypass otherwise-blocking pre-commit and pre-push hooks served by `wrix.prekHooks`
  [system](verify:prek.no-verify-bypasses-hooks)
- Heavy realization checks — those whose input closure includes the full sandbox base image or the full Rust workspace build — are referenced from a CI-only flake output (e.g., `.#test-ci`), not from `flake.nix#checks` which pre-push's `nix flake check` runs
  [check](verify:prek.ci-only-heavy-checks)
- Full image realization criteria use `test-ci:<app>` targets instead of the generic `verify:` registry; pre-push runs those targets on Linux and reports a policy skip without realizing them on Darwin
  [check](verify:prek.ci-only-heavy-checks)

## Requirements

### Functional

1. **Bundle ownership** — `wrix.prekHooks` owns every staged hook shim; consumers do not vendor or override unless they substitute the whole bundle via `mkDevShell { prekHooks = <derivation>; }`.
2. **`core.hooksPath` management** — The bundle is the hook path consumed by Wrix-owned install surfaces. `profiles.md` owns devshell installation and `cli.md` owns `wrix init`; consumers do not run `prek install` or maintain hook shims themselves.
3. **Hook stages** — the shim bundle covers pre-commit, pre-push, prepare-commit-msg, post-checkout, and post-merge; Wrix's own `.pre-commit-config.yaml` configures only pre-commit (treefmt, shell re-exec guard, builtin hooks) and pre-push (`nix flake check`, `loom gate verify`). The pre-push Loom invocation exports `WRIX_PRE_PUSH=1`, allowing the `test-ci:` runner to retain CI-only realizations on Linux while skipping them by default on Darwin.
4. **Stamp-file dance** — pre-push writes `.wrix/push-verified` after a successful check so a subsequent push can short-circuit if the SSH connection died during validation. See § Pre-Push Stamp Dance for the full mechanic.
5. **Marker-aware short-circuit** — each pre-push entry supplies stable hook id, entry, and range metadata through `bin/pre-push-checks`; the wrapper consults `loom gate verify-marker` to skip only the exact covered command.
6. **Graceful degrade in wrappers** — `pre-push-checks` executes the wrapped command when the marker, hook id, or Loom binary is absent, or when marker validation fails. `skip-if-missing` exits 0 silently when `<tool>` is absent from `PATH`. Neither wrapper exits non-zero on a missing-input path.
7. **Container hook parity** — profile containers install `core.hooksPath` and both wrappers on `PATH` so `.pre-commit-config.yaml` fires equivalently inside the container and on the host.

### Non-Functional

1. **`--no-verify` honored** — git's standard hook-bypass flag is the documented escape hatch when a hook would otherwise block an emergency commit or push.
2. **Schema independence** — Wrix's coupling to Loom is bounded by `.loom/marker.json` plus the hook-id, hook-entry, push-range, and exit-code contract of `loom gate verify-marker`. Marker schema and validation evolution do not require a Wrix change.
3. **No new runtime dependencies** — both wrappers are Bash scripts packaged by Wrix and use no external runtime utilities beyond commands already present in the devShell and profile-image base layer.
4. **Platform-aware pre-push cost** — The pre-push chain is the interactive critical path of `git push`. `nix flake check` covers only the fast `flake.nix#checks` subset on every platform. Heavier checks live under `.#test-ci`; the `test-ci:` Loom runner keeps those checks in Linux pre-push but skips them by default during Darwin pre-push, where realizing Linux image closures is disproportionately expensive. Running `nix run .#test-ci` directly or invoking Loom outside the pre-push environment still exercises them on Darwin. The marker short-circuit in `pre-push-checks` and the `wrix-base-image` chaining in `image-builder.md` reduce repeated warm-cache cost.

## Out of Scope

- Custom per-feature hook overrides — use a project-local `.pre-commit-config.yaml` patch
- Hook retry logic
- Parallel hook execution
- Parameterized `mkPrekHooks` constructor — consumers needing a different shim set substitute a hand-built derivation via `mkDevShell { prekHooks = <derivation>; }`.
- Cross-process serialization of prek hook execution — concurrent writers to the same working tree are outside wrix's hook bundle contract.
- `marker.json` schema, mint, and validation — owned by downstream loom.
- `loom gate verify-marker` subcommand internals — owned by downstream loom.
- `.pre-commit-config.yaml` content in downstream projects — wrix ships the wrappers, not the hook list.
- `push-verified` stamp deprecation/removal — orthogonal to `pre-push-checks`.
- A wrix-owned hook-id skip list for profiles that omit tools — optional dependencies are handled via `skip-if-missing` at the point of use, not via a wrix-side filter.
