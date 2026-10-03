# Pre-commit Hooks

Staged git hooks via [prek](https://github.com/j178/prek), packaged as a Nix-store bundle so consumers don't vendor shims, run `prek install`, or manage `core.hooksPath` themselves.

## Problem Statement

A wrix consumer using prek wants the same hook chain to fire in every context — host devshell, initialized host checkout, profile container, Loom integration clone, and agent-driven bead clone — without per-context shim files, manual `prek install` steps, or consumer-owned `core.hooksPath` book-keeping. Wrix ships one frozen Nix-store hook bundle and threads it through Wrix-owned install paths so the consumer's `.pre-commit-config.yaml` is the only place hooks are configured.

## Architecture

All git hooks for prek-using wrix repositories are served from a single content-addressed Nix-store derivation — `wrix.prekHooks` — pointed at by `core.hooksPath`. Its five stage shims and shared binding helper contain no platform-specific executable paths, so Darwin hosts and Linux workers select the same bundle. Each shim resolves a platform-native packaged runner before injecting its Nix-pinned prek, Git, and utility directories into `PATH`; an active devshell is not required. The pre-commit and post-* shims `exec` prek, while pre-push retains the stamp dance described below:

| Stage | Shim behavior |
|-------|---------------|
| pre-commit | `exec prek hook-impl --hook-type=pre-commit ...` |
| pre-push | `prek hook-impl --hook-type=pre-push ...` with `.wrix/push-verified` stamp dance |
| prepare-commit-msg | `exec prek hook-impl --hook-type=prepare-commit-msg ...` |
| post-checkout | `exec prek hook-impl --hook-type=post-checkout ...` |
| post-merge | `exec prek hook-impl --hook-type=post-merge ...` |

Each shim uses `prek hook-impl --hook-type=<stage>` rather than `prek run` because git passes positional args that `prek run` would mistake for hook/project selectors.

`profiles.md` § Prek hook management owns devshell installation, `cli.md` owns repository-scoped `wrix init` installation for ordinary host Git and Loom clones, and `image-builder.md` § Hook Installation owns profile-image and container-entrypoint installation. This spec owns the shared bundle and its behavior after those surfaces select it.

Consumers do not vendor shims, do not set `core.hooksPath` themselves, and do not run `prek install`. Git never reads `.git/hooks/` while `core.hooksPath` is set, so whatever lands there (e.g. `bd hooks install`) is inert — no chmod-lockdown is needed and no `prek install -f` runs from any wrix lifecycle.

## Installation and Runtime Resolution

Wrix-managed installation binds the Nix-resolved `wrix-prek` executable before
selecting `core.hooksPath`. The repository-local Git key is
`wrix.prek-<context>-<system>.runner`, where `<system>` is the executing Nix
platform (for example `aarch64-darwin` or `x86_64-linux`) and `<context>` is
`host` or `container`. `uname` normalizes Darwin `arm64` to `aarch64`. The
context defaults to container when `/etc/wrix/image-agent` exists, otherwise
host; profile images explicitly set `WRIX_PREK_CONTEXT=container`.
[test](../crates/wrix-cli/tests/prek_runtime.rs::worker_binding_preserves_host_and_foreign_platform_bindings)

The binding lives in common repository config, not a worktree-relative file.
Linked worktrees share it; independent integration and bead clones need their
own installation. An installer updates only its platform/context binding, so a
Linux worker does not replace a Darwin binding or even a same-platform host
binding. The canonical shared hook bundle remains `core.hooksPath` in every
context.
[test](../crates/wrix-cli/tests/prek_runtime.rs::linked_worktree_uses_common_binding_but_clone_needs_its_own)

Hooks need Bash, Git, and `uname` on the bootstrap PATH, but not Wrix or prek.
They read only the matching repository-local binding and execute its absolute
runner as `--print-bin-dir`. The runner supplies its packaged runtime PATH;
hook argv, stdin, policy, and exit status retain the stage-specific behavior.
No Wrix execution lock is introduced; upstream prek locking is unchanged.
[test](../crates/wrix-cli/tests/prek_runtime.rs::every_packaged_stage_runs_without_devshell_path)

For compatibility with installers that only select the bundle, an absent
binding can resolve packaged `wrix-prek` on PATH. An existing invalid/stale
binding never falls back to another platform/context or an ambient runner.
Missing bootstrap tools, runner, or packaged runtime fail with repair guidance,
not a silent hook skip. Optional consumer tools still use explicit
`skip-if-missing` policy.
[test](../crates/wrix-cli/tests/prek_runtime.rs::stale_binding_does_not_fall_back_to_ambient_runner)

Reloading the updated Wrix devshell repairs the current binding even when
`core.hooksPath` already matches. Packaged `wrix init` repairs the binding and
stale hook path when hook policy is enabled; `--no-hooks` remains a passive
opt-out. Worker entrypoints bind their image-native runner before installing
hooks. Existing worker images need rebuilding after a Wrix pin update.
[test](../crates/wrix-cli/tests/prek_runtime.rs::init_repairs_stale_runner_and_hook_path)

Repair each independent clone in its own directory: re-enter its updated
Wrix devshell, or run the updated Nix-packaged `wrix init` with the existing
repository key/signing policy. No key provisioning or global installation is
needed. A runner path collected by Nix GC is repaired the same way. Ordinary
Git operations thereafter need no active devshell.
[test](../crates/wrix-cli/tests/prek_runtime.rs::devshell_entry_repairs_binding_even_when_hook_path_is_current)

## Pre-Push Stamp Dance

The pre-push shim captures Git's remote name and location arguments plus the complete ordered ref transaction from standard input. It derives an approval identity from those values, the current HEAD object ID, and every local and remote ref name and object ID. After prek's checks pass, the shim writes that identity to `.wrix/push-verified` and exits 0. If the connection died during the checks, re-running the exact push transaction can short-circuit on a fresh connection.

The stamp is single-use: the next pre-push invocation consumes it whether or not its approval identity matches. Only an exact match short-circuits; a different HEAD, remote identity, ref name, ref object ID, or ref set runs the full checks. The hook cannot observe whether the network push ultimately succeeded, so an unconsumed exact-transaction approval may persist indefinitely and survive either push outcome. It has no expiry and does not prove an SSH failure or identify one network attempt; its safety comes from exact transaction binding and one-use consumption rather than age.

## Hook-Entry Wrappers

`wrix.prePushChecks` and `wrix.skipIfMissing` are sibling `writeShellScriptBin` derivations that prek hook `entry:` lines can name. `profiles.md` owns host devshell PATH exposure, while `image-builder.md` owns profile-image PATH exposure. The `prePushChecks` derivation packages the same script that this repository exposes at `bin/pre-push-checks`. Loom-managed repositories invoke the repo-local path so Loom can parse stable per-hook marker metadata without relying on ambient `PATH`; other consumers can use the packaged command.

### `pre-push-checks`

Wraps a slow check with a marker-aware, per-hook short-circuit. Contract:

```
bin/pre-push-checks --hook-id <id> --hook-entry <entry> [--push-range <range>] -- <command> [args…]
```

Resolution order:

1. If `.loom/marker.json` is absent in the current working directory, execute the wrapped command.
2. If the hook id or explicit entry metadata is absent, execute the wrapped command without a marker shortcut.
3. Resolve an omitted push range from the current upstream, falling back to the literal `@{u}..HEAD` range when no upstream is configured.
4. If `loom` is missing from `PATH`, execute the wrapped command.
5. Otherwise, invoke `loom gate verify-marker` with the hook id, hook entry, and push range:
   - exit 0 → wrapper exits 0 without running the wrapped command.
   - exit non-zero → execute the wrapped command.

The wrapper does **not** read or interpret `marker.json`. Schema, mint, and validation are owned by the downstream Loom project; Wrix supplies the hook identity, entry, and range to `loom gate verify-marker` and acts on its exit code.

Marker-aware entries repeat the exact wrapped command as explicit `--hook-entry` metadata. `loom gate verify-marker` is not a standalone pre-push hook: each slow hook asks the wrapper whether a marker covers that exact command and falls through normally when it does not.

### `skip-if-missing`

Wraps a check whose required tool may not be on `PATH` in every context. Contract:

```
skip-if-missing <tool> -- <command> [args…]
```

If `<tool>` resolves on `PATH`, `exec` the command. If absent, exit 0 silently. The wrapper keeps hooks inert in contexts that omit an optional runtime dependency. Knowledge of "this hook needs `<tool>`" lives in the hook entry, next to the command — not in a wrix-curated skip list.

### Relationship to `push-verified`

The `.wrix/push-verified` stamp is an exact-transaction, one-use approval: the pre-push shim writes it automatically after successful checks, but cannot know the later network outcome. `pre-push-checks` is a different layer — a per-entry opt-in skip when an external loom run has already validated the commit. Both can be active simultaneously. The stamp is not evidence of SSH failure and is not bounded to one network attempt.

## Hook Installation in Profile Containers

`image-builder.md` § Hook Installation owns the profile-image closure, wrapper PATH exposure, and entrypoint configuration of `core.hooksPath`, including linked-worktree handling. This spec owns the resulting hook behavior: configured pre-commit and pre-push stages dispatch through the shared bundle, and a failing hook aborts the Git operation. Optional tools remain explicit at the point of use through `skip-if-missing` rather than a wrix-side hook-id skip list.

## Success Criteria

- The `wrix.prekHooks` derivation contains executable shims for `pre-commit`, `pre-push`, `prepare-commit-msg`, `post-checkout`, and `post-merge`
  [check](verify:prek.bundle-contents)
- The pre-commit and pre-push shims both invoke `prek hook-impl --hook-type=<stage>` (not `prek run`, which would mistake git's positional args for hook/project selectors)
  [system](verify:prek.shims-use-hook-impl)
- No shim sources `lock.sh`, calls `_prek_acquire_lock`, or invokes `flock`; every shim invokes `prek hook-impl --hook-type=<its-stage>` through its packaged runtime
  [system](verify:prek.shims-no-flock)
- All five real packaged stage shims execute with a minimal PATH lacking both Wrix and prek
  [test](../crates/wrix-cli/tests/prek_runtime.rs::every_packaged_stage_runs_without_devshell_path)
- All five stage shims propagate configured hook failure, and failed pre-push checks do not mint approval stamps
  [test](../crates/wrix-cli/tests/prek_runtime.rs::every_packaged_stage_propagates_hook_failure)
- Real Git commit and push dispatch through the installed canonical bundle outside a devshell
  [test](../crates/wrix-cli/tests/prek_runtime.rs::git_commit_and_push_use_installed_bundle_with_minimal_path)
- Missing bindings fail with actionable reload/init instructions
  [test](../crates/wrix-cli/tests/prek_runtime.rs::missing_binding_fails_with_repair_instructions)
- Missing bootstrap dependencies produce actionable failures
  [test](../crates/wrix-cli/tests/prek_runtime.rs::missing_bootstrap_dependency_reports_actionable_failure)
- A runner missing its packaged prek dependency fails with repair guidance through the real hook/runtime seam
  [test](../crates/wrix-cli/tests/prek_runtime.rs::broken_runtime_reports_actionable_failure)
- Installation rejects missing packaged dependencies before changing the runner binding
  [test](../crates/wrix-cli/tests/prek_runtime.rs::binding_rejects_missing_packaged_dependency_before_mutation)
- Older bundle-only installations remain usable with a packaged runner on PATH
  [test](../crates/wrix-cli/tests/prek_runtime.rs::legacy_installation_can_use_packaged_runner_on_path)
- Synthetic foreign-platform execution cannot consume the native platform binding; this is not live Darwin verification
  [test](../crates/wrix-cli/tests/prek_runtime.rs::synthetic_foreign_platform_does_not_use_native_binding)
- The pre-push shim writes `.wrix/push-verified` after successful checks, binding the approval to the current HEAD, the remote name and location, and the complete ordered ref transaction; the next exact transaction consumes the stamp and skips checks once
  [system](verify:prek.pre-push-stamp)
- A same-HEAD invocation with a different remote name, remote location, local or remote ref name, local or remote object ID, or ref set consumes the old stamp and runs the checks
  [system](verify:prek.pre-push-stamp-transaction-scope)
- A stamp for a different HEAD is removed before checks run, and failed checks leave no approval stamp
  [system](verify:prek.pre-push-stale-stamp)
- Returning to a previously stamped HEAD after an intervening HEAD failed its checks runs the checks again rather than reviving the old approval
  [system](verify:prek.pre-push-stamp-cannot-revive)
- `wrix.prePushChecks` and `wrix.skipIfMissing` are exposed by the wrix library
  [check](verify:prek.wrappers-on-devshell-path)
- `pre-push-checks` passes the hook id, entry, and push range to `loom gate verify-marker` and exits 0 without running the wrapped command when marker validation succeeds
  [system](verify:prek.pre-push-checks-marker-valid)
- Marker content is opaque to Wrix: `pre-push-checks` delegates validation to Loom without parsing `.loom/marker.json`
  [system](verify:prek.pre-push-checks-marker-valid)
- `pre-push-checks` execs the wrapped command when `.loom/marker.json` is present and `loom gate verify-marker` exits non-zero
  [system](verify:prek.pre-push-checks-marker-stale)
- `pre-push-checks` execs the wrapped command when `.loom/marker.json` is absent
  [system](verify:prek.pre-push-checks-no-marker)
- `pre-push-checks` execs without consulting Loom when the hook id or explicit entry metadata is absent
  [system](verify:prek.pre-push-checks-no-metadata)
- `pre-push-checks` execs the wrapped command when `loom gate verify-marker` is not on `PATH`
  [system](verify:prek.pre-push-checks-no-loom)
- `skip-if-missing <tool> -- <cmd>` execs `<cmd>` when `<tool>` resolves on `PATH`
  [system](verify:prek.skip-if-missing-present)
- `skip-if-missing <tool> -- <cmd>` exits 0 without running `<cmd>` when `<tool>` is absent from `PATH`
  [system](verify:prek.skip-if-missing-absent)
- The wrappers add no utility dependencies of their own: `pre-push-checks` needs only Bash and Git before handing control to optional Loom or the wrapped command, and `skip-if-missing` needs only Bash before handing control to the probed or wrapped command
  [system](verify:prek.wrapper-runtime-dependencies)
- A pre-commit hook configured in `.pre-commit-config.yaml` fires when `git commit` runs inside a profile container
  [system](test-ci:test-container-pre-commit)
- A pre-push hook configured in `.pre-commit-config.yaml` fires when `git push` runs inside a profile container
  [system](test-ci:test-container-pre-push)
- `git commit --no-verify` and `git push --no-verify` bypass otherwise-blocking pre-commit and pre-push hooks served by `wrix.prekHooks`
  [system](verify:prek.no-verify-bypasses-hooks)
- Heavy realization checks — those whose input closure includes the full sandbox base image or the full Rust workspace build — are referenced from a CI-only flake output (e.g., `.#test-ci`), not from the fast `flake.nix#checks` set
  [check](verify:prek.ci-only-heavy-checks)
- Full image realization criteria use `test-ci:<app>` targets instead of the generic `verify:` registry
  [check](verify:prek.ci-only-heavy-checks)
- Pre-push runs `test-ci:` targets on Linux and reports a policy skip without realizing them on Darwin, while direct Darwin invocation still runs them
  [system](verify:prek.ci-platform-policy)

## Requirements

### Functional

1. **Bundle ownership** — `wrix.prekHooks` owns every staged hook shim; consumers do not vendor or override unless they substitute the whole bundle via `mkDevShell { prekHooks = <derivation>; }`.
2. **`core.hooksPath` management** — The bundle is the hook path consumed by Wrix-owned install surfaces. `profiles.md` owns devshell installation and `cli.md` owns `wrix init`; consumers do not run `prek install` or maintain hook shims themselves.
3. **Hook stages** — the shim bundle covers pre-commit, pre-push, prepare-commit-msg, post-checkout, and post-merge.
4. **Stamp-file dance** — pre-push writes `.wrix/push-verified` after a successful check as a one-use approval for the exact current HEAD, remote identity, and pushed ref transaction. The approval has no expiry, survives either eventual network outcome, and does not prove the connection died. See § Pre-Push Stamp Dance for the full mechanic.
5. **Marker-aware short-circuit** — each pre-push entry supplies stable hook id, entry, and range metadata through `bin/pre-push-checks`; the wrapper consults `loom gate verify-marker` to skip only the exact covered command.
6. **Graceful degrade in wrappers** — `pre-push-checks` executes the wrapped command when the marker, hook id, or Loom binary is absent, or when marker validation fails. `skip-if-missing` exits 0 silently when `<tool>` is absent from `PATH`. Neither wrapper exits non-zero on a missing-input path.
7. **Container hook parity** — once installed through the image-builder-owned surface, the shared bundle dispatches configured hooks equivalently inside the container and on the host.

### Non-Functional

1. **`--no-verify` honored** — git's standard hook-bypass flag is the documented escape hatch when a hook would otherwise block an emergency commit or push.
2. **Schema independence** — Wrix's coupling to Loom is bounded by `.loom/marker.json` plus the hook-id, hook-entry, push-range, and exit-code contract of `loom gate verify-marker`. Marker schema and validation evolution do not require a Wrix change.
3. **Runtime dependency boundary** — `pre-push-checks` uses Bash and Git before handing control to Loom or the wrapped command; `skip-if-missing` uses Bash before handing control to the probed or wrapped command.
4. **Platform-aware pre-push cost** — The pre-push chain is the interactive critical path of `git push`. CI-only realization targets run during Linux pre-push and report a policy skip during Darwin pre-push, where realizing Linux image closures is disproportionately expensive. Direct CI-only invocation still exercises them on Darwin.

## Out of Scope

- Custom per-feature hook overrides — use a project-local `.pre-commit-config.yaml` patch
- Hook retry logic
- Parallel hook execution
- Parameterized `mkPrekHooks` constructor — consumers needing a different shim set substitute a hand-built derivation via `mkDevShell { prekHooks = <derivation>; }`.
- Cross-process serialization of prek hook execution — concurrent writers to the same working tree are outside wrix's hook bundle contract.
- `marker.json` schema, mint, and validation — owned by downstream loom.
- `loom gate verify-marker` subcommand internals — owned by downstream loom.
- Specific hook commands in Wrix's or downstream projects' `.pre-commit-config.yaml` — wrix ships the wrappers, not the hook list.
- `push-verified` stamp deprecation/removal — orthogonal to `pre-push-checks`.
- A wrix-owned hook-id skip list for profiles that omit tools — optional dependencies are handled via `skip-if-missing` at the point of use, not via a wrix-side filter.
