# Pre-commit Hooks and Ralph Run Integration

Unified hook system for git workflow validation and ralph run automation.

## Problem Statement

Current state:
- ~~Basic prek setup exists but lacks stage separation (fast vs slow checks)~~ Done
- Ralph loop hooks (`pre-hook`, `post-hook`) defined in config but not implemented
- LLMs may skip quality gates defined in templates
- No enforcement mechanism for tests/linting between steps
- "Land the plane" protocol is manual and error-prone

### Hook Ownership

Hook stages are served from two locations:

- **`pre-commit` and `pre-push`** are served from `lib/prek/hooks/` via `core.hooksPath` (see FR7). The versioned shims there acquire the serialization `flock` and then `exec prek run --hook-stage <stage>`. Git reads only from `core.hooksPath` for these stages, so anything under `.git/hooks/pre-commit` or `.git/hooks/pre-push` is inert.
- **All other stages** (`prepare-commit-msg`, `post-checkout`, `post-merge`) are served from `.git/hooks/`, written by `prek install`. Beads (bd) hooks run as prek-managed local hooks in `.pre-commit-config.yaml` via `bd hooks run <stage>`. The `.git/hooks/` directory is `chmod 555` to prevent `bd hooks install` from overwriting prek's shims for these non-FR7 stages.

To update the non-FR7 hook set:
```bash
chmod 755 .git/hooks/ && prek install -f && chmod 555 .git/hooks/
```

The FR7 shims are versioned in the repo and need no install step — they're effective as soon as `core.hooksPath` is set (see FR7 *Install ordering*).

## Requirements

### Functional Requirements

#### FR1: prek Stage Separation

Configure `.pre-commit-config.yaml` with staged hooks:

| Stage | Hooks | Purpose |
|-------|-------|---------|
| pre-commit | bd dolt pull, nixfmt, shellcheck, ralph check -t (template files only), builtin hooks | Fast validation on every commit |
| prepare-commit-msg | bd agent trailers | Add agent identity to commits |
| post-checkout | bd dolt pull | Pull Dolt state after branch switch |
| post-merge | bd dolt pull | Pull Dolt state after pull/merge |
| pre-push | bd stale check, nix flake check, tests | Slow validation before sharing |

Builtin hooks to add:
- `trailing-whitespace`
- `end-of-file-fixer`
- `check-merge-conflict`

#### FR2: Ralph Run Hook Points

Implement four hook points in ralph run:

```
ralph run [feature]
├── [pre-loop]     → Before any work starts
│
├── while has_work:
│   ├── [pre-step]  → Before each step
│   ├── step        → Claude works on one bead
│   └── [post-step] → After each step
│
└── [post-loop]     → After all work complete
```

#### FR3: Hook Configuration Schema

Update `config.nix` with simplified hook structure:

```nix
{
  hooks = {
    pre-loop = "prek run";
    pre-step = "bd dolt pull";
    post-step = "prek run";
    post-loop = ''
      bd dolt push
      git diff --quiet || { echo "Error: worktree is dirty; commit or stash before pushing" >&2; exit 1; }
    '';
  };

  hooks-on-failure = "block";  # block | warn | skip
}
```

#### FR4: Template Variable Substitution

Hooks support these variables:

| Variable | Description | Available In |
|----------|-------------|--------------|
| `{{LABEL}}` | Feature label | All hooks |
| `{{ISSUE_ID}}` | Current bead ID | pre-step, post-step |
| `{{STEP_COUNT}}` | Current iteration number | pre-step, post-step |
| `{{STEP_EXIT_CODE}}` | Exit code from ralph-step | post-step only |

#### FR5: Failure Handling

`hooks-on-failure` options:

| Action | Behavior |
|--------|----------|
| `block` | Stop loop, exit with error code |
| `warn` | Log warning to stderr, continue |
| `skip` | Silently continue |

Default: `block` (fail fast, require human intervention)

#### FR6: Run Template Update

Update `run.md` to reference hook enforcement:

```markdown
## Quality Gates
Before outputting RALPH_COMPLETE:
- [ ] Tests written and passing
- [ ] Lint checks pass
- [ ] Changes staged (`git add`)

Post-step hooks verify compliance automatically.
```

#### FR7: Hook Serialization

prek's stash/restore dance around unstaged changes can race with concurrent
writers, silently dropping working-tree edits (wx-m5yuq). To serialize hook
invocations across agents sharing a workspace, the installed pre-commit and
pre-push hooks acquire an exclusive `flock` on `.wrapix/prek.lock` before
invoking prek's hook run, and release it when prek exits. Both stages are
covered because `ralph check` routinely triggers pre-push at the end of every
ralph run, and concurrent ralph sessions would otherwise re-expose the same
race at push time.

- **Mechanism** — versioned shims at `lib/prek/hooks/pre-commit` and `lib/prek/hooks/pre-push` acquire the `flock` and then `exec` prek (`prek run --hook-stage <stage> "$@"`). Git is pointed at the shim directory via `core.hooksPath = lib/prek/hooks`, so the shims are the source of truth rather than generated artifacts under `.git/hooks/`. Shims share a single helper (`lib/prek/hooks/_flock.sh`) so the per-stage scripts stay one-liners
- **Lock file** — `.wrapix/prek.lock`, gitignored, auto-created on first use; located in the main repo's `.wrapix/` so every agent (host, container with `.wrapix` mount, linked worktree) shares the same lock. A single lock covers both stages because both protect the same working-tree stash window — a commit and a push must not interleave either
- **Path resolution from any worktree** — the shim resolves the lock path via `$(dirname "$(git rev-parse --git-common-dir)")/.wrapix/prek.lock`. `git rev-parse --git-common-dir` always returns the main repo's `.git/` from any linked worktree, so commits fired from a linked worktree (or a gascity worker's worktree, if the shim happens to run there) share the same lock as the main repo — no relative-path surprises
- **Timeout** — `flock --timeout 600` (10 min) for both stages. Rationale: pre-push runs `nix flake check` plus the integration suites and has been observed to exceed 5 min legitimately, so the timeout must outlast the slowest real job rather than the median; 10 min still bounds genuinely stuck holders. If the timeout fires, the shim fails loudly (`>&2` with lock-holder PID if obtainable) and exits non-zero so the commit or push aborts rather than deadlocking the workspace
- **Scope** — every commit and push routed through the installed hooks participates: manual user commits/pushes, ralph run quality-gate commits, `ralph check` pre-push invocations, and `beads-push` commits. Two explicit out-of-scope cases:
  - **Gascity workers** — each worker already operates in its own linked worktree (`.wrapix/worktree/<bead-id>` — see [gas-city.md](gas-city.md)), so prek inside a worker has no concurrent writer to race against. Whether gascity-path commits incidentally hit the shim via `core.hooksPath` is harmless but not load-bearing.
  - **Ralph-internal `prek run` invocations** — FR3's `pre-step`/`post-step` hooks invoke `prek run` directly as validation/lint steps, not as git hooks, so they don't route through the shim. This is safe because ralph run's loop is single-threaded per label — only one `prek run` call is in flight at a time within a ralph session. Concurrent ralph sessions on the same workspace are covered at the commit boundary instead (the quality-gate commit goes through the shim)
- **`--no-verify` bypass** — skips the hook and therefore the lock, but also skips prek's stash, so no race exists on that path. The documented escape hatch for emergency pushes
- **`flock` availability** — `flock(1)` is a hard requirement, not a graceful-degrade. The shim aborts with a clear error (`flock not found — enter nix develop or install util-linux`) if `flock` is missing from `PATH`. `flock` is added to the nix devShell and to the wrapix sandbox `base` profile so every context that might invoke the shim (devShell, every wrapix container, including gascity worker containers where the shim may incidentally run) has it available without per-user setup. macOS hosts running `git commit` outside `nix develop` are the only environment that can hit the missing-flock path, and the error message points them at the fix
- **Install ordering** — `core.hooksPath` is set idempotently by the `flake.nix` devShell `shellHook` on every `nix develop` entry (`git config --local core.hooksPath lib/prek/hooks`). No per-user setup step is required, and the shims survive subsequent `prek install` runs because they live outside `.git/hooks/`. The `chmod 555 .git/hooks/` rule in Hook Ownership still applies to `.git/hooks/` but is now redundant for the pre-commit and pre-push paths (since Git reads the shims from `core.hooksPath` instead). Contributors who commit outside `nix develop` and have never entered the devShell on this clone won't have `core.hooksPath` set and bypass FR7 entirely — this is considered acceptable (rare, outside the concurrent-agent scenario); contributors who *have* entered the devShell but then commit outside it still have `core.hooksPath` set in their local git config and hit the *flock availability* bullet above instead (aborting loudly rather than silently racing)

### Non-Functional Requirements

#### NFR1: Consumer Repo Assumptions

- Wrapix is a library; consumer repos have their own test setups
- Assume consumer repos have prek installed and configured
- Default hooks use `prek run`, not repo-specific test commands

#### NFR2: LLM-Friendly Output

Consumer repos should configure their test runners with quiet mode for LLM environments:
- Default: Show only failures
- `--verbose` flag: Full output for human operators
- Detection via `RALPH_MODE` environment variable (optional)

## Affected Files

| File | Changes |
|------|---------|
| `.pre-commit-config.yaml` | Add stages, builtin hooks |
| `lib/ralph/cmd/run.sh` | Implement hook execution |
| `lib/ralph/template/config.nix` | New hooks schema |
| `lib/ralph/template/run.md` | Update quality gates section |
| `lib/prek/hooks/pre-commit` | `flock`-wrapped prek entry point for pre-commit stage (FR7) |
| `lib/prek/hooks/pre-push` | `flock`-wrapped prek entry point for pre-push stage (FR7) |
| `lib/prek/hooks/_flock.sh` | Shared helper: resolve main-repo lock path and take the lock |
| `.gitignore` | Ignore `.wrapix/prek.lock` |
| `flake.nix` devShell shellHook | Set `core.hooksPath = lib/prek/hooks` idempotently on entry (FR7) |
| `flake.nix` devShell | Add `flock` (util-linux) to devShell packages (FR7) |
| `lib/sandbox/profiles.nix` (base profile) | Add `flock` (util-linux) to base profile packages (FR7) |
| `tests/ralph/run-tests.sh` | Update hook tests (remove skip) |
| `tests/ralph/scenarios/hook-test.sh` | Expand test coverage |

## Success Criteria

- [ ] `prek run` executes only fast hooks; slow hooks run on `git push`
  [judge](../tests/judges/pre-commit.sh#test_prek_hook_speed_split)
- [ ] Ralph loop executes hooks at all four points
  [verify](../tests/ralph/run-tests.sh#test_default_config_has_hooks)
- [ ] Loop pauses on hook failure when `hooks-on-failure = "block"`
  [verify](../tests/ralph/run-tests.sh#test_config_data_driven)
- [ ] Existing tests pass; hook tests no longer skipped
  [verify](../tests/ralph/run-tests.sh#test_default_config_has_hooks)
- [ ] Template variables substituted correctly in hook commands
  [verify](../tests/ralph/run-tests.sh#test_render_template_basic)
- [ ] Two concurrent `git commit` invocations serialize via `.wrapix/prek.lock`; neither loses working-tree edits (wx-m5yuq repro)
  [verify](../tests/ralph/run-tests.sh#test_prek_lock_commit_serializes)
- [ ] Two concurrent `git push` invocations serialize via `.wrapix/prek.lock`
  [verify](../tests/ralph/run-tests.sh#test_prek_lock_push_serializes)
- [ ] Commit against a held push lock waits up to 10 min, then fails loudly with a clear error rather than hanging
  [verify](../tests/ralph/run-tests.sh#test_prek_lock_cross_stage_timeout)
- [ ] Both pre-commit and pre-push shims use a 10-min `flock` timeout
  [verify](../tests/ralph/run-tests.sh#test_prek_lock_timeout)
- [ ] `git commit --no-verify` and `git push --no-verify` bypass both the lock and prek (documented escape hatch)
  [verify](../tests/ralph/run-tests.sh#test_prek_lock_no_verify_bypass)
- [ ] Shim aborts with a clear `flock not found` error when `flock` is missing from `PATH`
  [verify](../tests/ralph/run-tests.sh#test_prek_lock_requires_flock)
- [ ] `flock` is present in the nix devShell and the sandbox `base` profile
  [verify](../tests/ralph/run-tests.sh#test_flock_in_devshell_and_base_profile)
- [ ] Entering the nix devShell sets `core.hooksPath = lib/prek/hooks` idempotently
  [verify](../tests/ralph/run-tests.sh#test_devshell_sets_core_hooks_path)
- [ ] `git commit` routes through `lib/prek/hooks/pre-commit` (shim actually intercepts the commit)
  [verify](../tests/ralph/run-tests.sh#test_git_commit_routes_through_shim)

## Out of Scope

- Custom per-feature hook overrides (use config.nix)
- Hook retry logic (may add later)
- Parallel hook execution
- Test runner quiet mode implementation (repo-specific)
