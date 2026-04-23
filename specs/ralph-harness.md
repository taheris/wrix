# Ralph Harness

Platform for ralph: state, templates, containers, utilities, and bootstrap.

## Problem Statement

The ralph workflow is a pipeline of focused AI sessions (plan → todo → run → check,
with clarify resolution via msg). Each of those commands needs common platform
services: per-label state files, template rendering, container execution, session
re-pinning under auto-compaction, project configuration, and bootstrap. This spec
defines the platform those commands share. Pipeline semantics live in
[ralph-loop.md](ralph-loop.md); review + clarify semantics live in
[ralph-review.md](ralph-review.md).

## Requirements

### Functional

1. **Concurrent Workflows** — Per-label state files (`state/<label>.json`) replace singleton `state/current.json`, enabling multiple ralph workflows simultaneously
2. **Spec Switching** — `ralph use <name>` sets the active workflow; `--spec <name>` flag on pipeline commands targets a specific workflow
3. **Status & Logs** — `ralph status` shows molecule progress; `ralph logs` finds errors and shows context
4. **Template Validation** — `ralph check -t` validates all templates and partials; also wired into `nix flake check`
5. **Template Tuning** — `ralph tune` edits templates (interactive or integration mode)
6. **Template Sync** — `ralph sync` updates local templates (use `--diff` to preview changes)
7. **Container Execution** — Claude-calling commands (`plan`, `todo`, `run --once`, `check`, interactive `msg`) run inside wrapix containers; utility commands run on host
8. **Container Bead Sync** — Container-executed commands run `bd dolt push` inside the container after `RALPH_COMPLETE` before exit, then the host runs `bd dolt pull` after container exits
9. **Compaction Re-Pin** — Container-executed Claude sessions register a `SessionStart` hook with matcher `"compact"` that re-injects a condensed orientation (label, spec path, exit signals, command-specific context) after auto-compaction, so the model retains orientation without re-pinning the full spec body
10. **`ralph:clarify` Label** — Beads waiting on human response carry this label; the run loop filters them out, a notification is emitted when first applied, and `ralph msg` handles resolution (see [ralph-review.md](ralph-review.md))
11. **Project Bootstrap** — `ralph init` scaffolds a fresh wrapix project from zero (`flake.nix`, `.envrc`, `.gitignore`, `.pre-commit-config.yaml`, `bd init`, `docs/`, `AGENTS.md`, `CLAUDE.md` symlink, `.wrapix/ralph/template/`). Invocable as a flake app (`nix run github:taheris/wrapix#init`) for use before ralph is on PATH. All artifacts are skip-if-exists; no git operations. Shares `scaffold_docs`/`scaffold_agents`/`scaffold_templates` with `ralph sync`

### Non-Functional

1. **Context Efficiency** — Each step starts with minimal, focused context
2. **Resumable** — Work can stop and resume across sessions
3. **Observable** — Clear visibility into current state and progress via molecules
4. **Validated** — Templates statically checked at build time and after edits
5. **Isolated** — Claude-calling commands run inside wrapix containers for security and reproducibility

## Workflow Phases

```
plan → todo → run → check → (done + push)
  │       │     │      │        │
  │       │     │      │        └─ git push + beads-push (only on RALPH_COMPLETE + no new beads)
  │       │     │      ├─ Invariant-clash detection → ralph:clarify (stop, wait for ralph msg)
  │       │     │      ├─ Fix-up beads found → exec ralph run (auto-iterate)
  │       │     │      └─ Reviewer reads code + spec; bonds fix-ups to molecule
  │       │     ├─ Implementation + bd mol bond (discovered work); commits per-bead, no push
  │       │     └─ At molecule completion → exec ralph check
  │       └─ Molecule creation from specs/<label>.md
  └─ Spec interview → writes specs/<label>.md
     ├─ "one by one" → guided walk through open questions with defaults
     └─ "polish the spec" → end-of-session read-through for consistency / ambiguity
```

See [ralph-loop.md](ralph-loop.md) for the forward pipeline (plan/todo/run) and
[ralph-review.md](ralph-review.md) for the review gate (check/msg) including the
auto-iteration loop and clarify resolution sub-diagrams.

## Container Execution

Ralph runs Claude-calling commands inside wrapix containers for isolation and reproducibility.

| Command | Execution | Profile |
|---------|-----------|---------|
| `ralph init` | host | N/A (bootstrap; creates `flake.nix` so must precede any container) |
| `ralph plan` | wrapix container | base |
| `ralph todo` | wrapix container | base |
| `ralph run` | host (orchestrator) | N/A (spawns containerized work per-issue) |
| `ralph run --once` | wrapix container | from bead label or `--profile` flag (fallback: base) |
| `ralph status` | host | N/A (utility) |
| `ralph logs` | host | N/A (utility) |
| `ralph check` (default) | wrapix container | base |
| `ralph check -t` | host | N/A (utility) |
| `ralph msg` (interactive) | wrapix container | base |
| `ralph msg` view/fast-reply/dismiss (`-n`, `-i`, with or without `-a`/`-d`) | host | N/A (utility) |
| `ralph tune` | host | N/A (utility) |
| `ralph sync` | host | N/A (utility) |
| `ralph use` | host | N/A (utility) |

**Rationale:**
- `plan` and `todo` involve AI decision-making that benefits from isolation
- `run --once` performs implementation work requiring language toolchains
- `run` (continuous) is a simple orchestrator that spawns containerized steps
- `msg` interactive is a Claude Drafter session that needs codebase access to help the user decide among clarify options
- `msg` view, fast-reply, and dismiss are pure bead operations that don't need Claude — kept host-side for speed
- Utility commands don't invoke Claude and run directly on host

**Container bead sync protocol:** All container-executed commands (`plan`, `todo`, `run --once`, `check`, `msg` interactive) follow this exit sequence:
1. Command logic completes, outputs `RALPH_COMPLETE`
2. Container-side `<cmd>.sh` runs `bd dolt push` (syncs container `.beads/` → Dolt remote)
3. Container exits
4. Host-side `<cmd>.sh` runs `bd dolt pull` (syncs Dolt remote → host `.beads/`)

This is necessary because the container has its own `.beads/` database (not bind-mounted). Without the push/pull handoff, beads created inside the container are lost when the container exits. The host-side pull is the final step; if `bd dolt push` failed inside the container, the pull gets stale data and the host emits an informational warning with recovery hints.

## Commands

### `ralph status`

```bash
ralph status                # Show status for current spec (from state/current)
ralph status --spec <name>  # Show status for named spec
ralph status -s <name>      # Short form
ralph status --all          # Summary of all active workflows
```

Shows molecule progress for the resolved spec:
```
Ralph Status: my-feature
===============================
Molecule: bd-xyz123
Spec: specs/my-feature.md

Progress:
  [####------] 40% (4/10)

Current Position:
  [done]    Setup project structure
  [done]    Implement core feature
  [current] Write tests         <- you are here
  [ready]   Update documentation
  [blocked] Final review (waiting on tests)
```

**`--all` mode** shows a summary of all active workflows:
```
Active Workflows:
  my-feature      running  [####------] 40% (4/10)
  auth-refactor   todo     [----------]  0% (0/5)
  bugfix-123      done     [##########] 100% (3/3)
```

### `ralph logs`

```bash
ralph logs              # Find most recent error for current spec
ralph logs -n 50        # Show 50 lines of context before error
ralph logs --all        # Show full log without error filtering
ralph logs --spec <name>  # Show logs for named spec
ralph logs -s <name>      # Short form
```

Error-focused output: Scans for error patterns (exit code != 0, "error:", "failed"), shows context leading up to first match.

### `ralph tune`

**Interactive mode** (no stdin):
```bash
ralph tune
> What would you like to change?
> "Add guidance about handling blocked beads"
>
> Analyzing templates...
> This should go in run.md, section "Instructions"
>
> [makes edit to .wrapix/ralph/template/run.md]
> [runs ralph check]
> ✓ Template valid
```

**Integration mode** (stdin with diff):
```bash
ralph sync --diff | ralph tune
> Analyzing diff...
>
> Change 1/2: run.md lines 35-40
> + 6. **Blocked vs Waiting**: ...
>
> Where should this go?
>   1. Keep in run.md
>   2. Move to partial
>   3. Create new partial
> > 1
>
> Accept this change? [Y/n] y
> ✓ Change applied
```

AI-driven interview that asks questions until user accepts or abandons.

### `ralph sync`

```bash
ralph sync           # Update local templates from packaged
ralph sync --diff    # Show local template changes vs packaged (preview)
ralph sync --dry-run # Preview sync without executing
```

Synchronizes local templates with packaged versions:

1. Creates `.wrapix/ralph/template/` with fresh packaged templates
2. Moves existing customized templates to `.wrapix/ralph/backup/`
3. Copies all templates including variants and `partial/` directory
4. Scaffolds project documentation (skip-if-exists):
   - `docs/README.md` — spec index stub with Specs/Beads/Purpose table
   - `docs/architecture.md` — architecture overview stub
   - `docs/style-guidelines.md` — code style guidelines stub
   - `AGENTS.md` — agent instructions pointing at the docs above

**`--diff` mode**: Shows changes between local templates and packaged versions. Pipe to `ralph tune` for integration:
```bash
ralph sync --diff | ralph tune
```

**Directory structure after sync:**
```
.wrapix/ralph/
├── config.nix
├── template/            # Fresh from packaged
│   ├── partial/
│   │   ├── companions-context.md
│   │   ├── context-pinning.md
│   │   ├── exit-signals.md
│   │   ├── interview-modes.md
│   │   └── spec-header.md
│   ├── check.md
│   ├── msg.md
│   ├── plan-new.md
│   ├── plan-update.md
│   ├── run.md
│   ├── todo-new.md
│   └── todo-update.md
└── backup/              # User customizations (if any)
    └── ...
```

Use `ralph sync --diff` to see what changed, then `ralph tune` to merge customizations from backup.

### `ralph use`

```bash
ralph use <name>        # Switch active workflow
```

Sets `state/current` to the given label after validation:
1. Validates the spec exists (`specs/<name>.md` or hidden spec in `state/`)
2. Validates `state/<name>.json` exists (workflow must be initialized via `ralph plan`)
3. Writes the label to `state/current`
4. Errors with clear message if either validation fails

### `ralph check -t`

`ralph check -t` / `ralph check --templates` is the template validator; it does not invoke Claude and is runnable anywhere, also wired into `nix flake check`.

Validation:
- Partial files exist
- Body files parse correctly
- No syntax errors in Nix expressions
- Dry-run render with dummy values to catch placeholder typos

The default form of `ralph check` (without `-t`) is the post-loop reviewer — see [ralph-review.md](ralph-review.md).

### `ralph init`

Cold-start bootstrap for a new wrapix project. Creates the minimal files needed for `ralph plan` to be immediately useful. Invocable as a flake app so it works before ralph is on PATH.

```bash
nix run github:taheris/wrapix#init   # from a fresh directory
ralph init                            # from inside a devShell
```

**Scope:** always operates on `cwd`. No path argument, no `git init`, no initial commit, no remote setup.

**Execution context:** runs on the **host**, not in a container. `ralph init` creates `flake.nix` and `.envrc` — both must exist before any wrapix container can spin up. Running init inside a container would be a chicken-and-egg loop.

**Artifacts (all skip-if-exists, idempotent):**

| Artifact | Source | Skip condition |
|----------|--------|----------------|
| `flake.nix` | `lib/ralph/template/flake.nix` | file exists |
| `.envrc` | static `use flake\n` | file exists |
| `.gitignore` | append-missing of `.direnv/`, `.wrapix/`, `result`, `result-*` | all entries present |
| `.pre-commit-config.yaml` | `lib/ralph/template/pre-commit-config.yaml` | file exists |
| `.beads/` | `bd init` | `.beads/` directory exists |
| `docs/README.md`, `docs/architecture.md`, `docs/style-guidelines.md` | `scaffold_docs` (shared with sync) | file exists |
| `AGENTS.md` | `scaffold_agents` (shared with sync) | file exists |
| `CLAUDE.md` | `ln -sf AGENTS.md CLAUDE.md` | `CLAUDE.md` exists (file or symlink) |
| `.wrapix/ralph/template/` + `partial/` | `scaffold_templates` (shared with sync) | directory exists |

**Shared scaffolding:** `scaffold_docs`, `scaffold_agents`, `scaffold_templates` live in `lib/ralph/cmd/util.sh` (or `scaffold.sh`) and are called by both `ralph init` and `ralph sync`. Init-only helpers (`bootstrap_flake`, `bootstrap_envrc`, `bootstrap_gitignore`, `bootstrap_precommit`, `bootstrap_beads`, `bootstrap_claude_symlink`) are not shared.

**`flake.nix` template (`lib/ralph/template/flake.nix`):**

Uses flake-parts with an `apps.sandbox` app, a `devShells.default` composing `${ralph.shellHook}` (exposed as a passthru on wrapix's ralph package), and treefmt-nix integration. `checks.treefmt` is not declared explicitly — `inputs.treefmt-nix.flakeModule` registers it automatically, and defining it twice triggers a multiple-definition evaluation error:

```nix
{
  description = "wrapix project";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    treefmt-nix.url = "github:numtide/treefmt-nix";
    wrapix.url = "github:taheris/wrapix";
  };

  outputs = inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
      imports = [ inputs.treefmt-nix.flakeModule ];

      perSystem = { config, pkgs, system, ... }:
        let
          ralph = inputs.wrapix.packages.${system}.ralph;
        in {
          apps.sandbox = {
            type = "app";
            program = "${inputs.wrapix.lib.mkSandbox { inherit system; }}/bin/wrapix";
          };

          devShells.default = pkgs.mkShell {
            packages = [
              ralph
              inputs.wrapix.packages.${system}.bd
              config.treefmt.build.wrapper
            ];
            shellHook = ''
              ${ralph.shellHook}
            '';
          };

          treefmt = {
            projectRootFile = "flake.nix";
            programs = {
              deadnix.enable = true;
              nixfmt.enable = true;
              shellcheck.enable = true;
              statix.enable = true;
            };
            settings.formatter = {
              shellcheck.excludes = [ ".envrc" ];
            };
          };
        };
    };
}
```

`rustfmt` is intentionally omitted from the default template — it's the only treefmt program that carries a language assumption. Users enable it when they add Rust code. `mkCity` is not wired in; users opt in by extending their flake.

**`.pre-commit-config.yaml` template:**

```yaml
repos:
  - repo: local
    hooks:
      - id: treefmt
        name: treefmt
        entry: nix fmt
        language: system
        pass_filenames: false
      - id: nix-flake-check
        name: nix flake check
        entry: nix flake check
        language: system
        pass_filenames: false
        stages: [pre-push]
```

Formatting/linting runs on pre-commit (via treefmt, which covers deadnix/nixfmt/shellcheck/statix). `nix flake check` is pre-push only — too slow for every commit.

**Output format** — per-artifact summary plus next-steps block, printed on exit:

```
✓ Bootstrapped wrapix project in .

Created:
  flake.nix
  .envrc
  .pre-commit-config.yaml
  .gitignore  (4 entries appended)
  AGENTS.md
  CLAUDE.md (-> AGENTS.md)
  docs/README.md
  docs/architecture.md
  docs/style-guidelines.md
  .wrapix/ralph/template/
Skipped:
  .beads/  (already initialized)

Next steps:
  1. direnv allow            # devShell auto-enters
  2. ralph plan -n <label>   # start your first feature

Docs: specs/ralph-harness.md
```

Skipped entries list the reason in parentheses. The next-steps block is fixed (2 steps + docs pointer).

**Flake app exposure:** the top-level wrapix flake exposes `apps.init` so `nix run github:taheris/wrapix#init` invokes `ralph init` without requiring ralph to be pre-installed.

## `ralph:clarify` Label

Beads waiting on human response are tagged with `ralph:clarify`. Used by implementation workers and the reviewer agent. The run loop filters out beads with this label when selecting the next bead to work. Each iteration re-queries, so when a human removes the label via `ralph msg`, the bead becomes eligible on the next pass. A notification is emitted when the label is first applied.

The Options Format Contract — the markdown shape that `ralph msg` consumes and `ralph check` produces for invariant-clash beads — is defined in [ralph-review.md](ralph-review.md).

## Compaction Re-Pin

Claude Code auto-compacts long-running sessions when the context window fills. The initial rendered template content (label, spec path, companion manifest list, exit-signal instructions, issue details) can be pushed out of the compacted transcript, causing the model to drift — forgetting which spec it's working on, which exit signals to use, which companion files exist to consult.

Ralph configures a `SessionStart` hook with matcher `"compact"` so a condensed re-pin is re-injected into the session on the next model turn. The re-pin deliberately excludes the full spec body (the model can re-read `specs/<label>.md` on demand); keeping the injection small protects the savings from compaction.

### Hook Scope

The hook is registered for every container-executed Claude session. Host-side commands (`status`, `logs`, `check -t`, `tune`, `sync`, `use`, `init`, and non-interactive `msg` operations — view, fast-reply, dismiss) do not invoke Claude and do not register the hook.

| Command | Re-pin content |
|---------|----------------|
| `ralph plan` (new/update) | Label, spec path, mode (`new`/`update`), exit signals |
| `ralph todo` | Label, spec path, molecule ID (if set), companion paths, exit signals |
| `ralph run --once` | Label, spec path, issue ID, title, companion paths, exit signals |
| `ralph check` | Label, spec path, molecule ID, base commit, exit signals |
| `ralph msg` (interactive) | Label, spec path, outstanding clarify IDs + summaries, exit signals |

The re-pin does NOT include the full spec body, companion manifest bodies, full issue description, or task list — these can be re-read on demand from `specs/<label>.md`, the companion directories, `bd show <id>`, and `bd mol current`.

### Implementation

Per container-executed command, the command's shell script (e.g., `plan.sh`, `todo.sh`, `run.sh`, `check.sh`) writes two files under a per-label runtime directory before invoking wrapix:

```
.wrapix/ralph/runtime/<label>/
├── repin.sh              # Emits the condensed re-pin as JSON on stdout
└── claude-settings.json  # SessionStart hook fragment pointing at repin.sh
```

The hook fragment uses the `hookSpecificOutput.additionalContext` form so output is explicitly treated as injected context (plain stdout also works; JSON is preferred for clarity and the 10KB cap is well within ralph's needs):

```json
{
  "hookSpecificOutput": {
    "hookEventName": "SessionStart",
    "additionalContext": "Label: my-feature\nSpec: specs/my-feature.md\n..."
  }
}
```

The settings fragment shape:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "compact",
        "hooks": [
          { "type": "command", "command": "/workspace/.wrapix/ralph/runtime/<label>/repin.sh" }
        ]
      }
    ]
  }
}
```

### Settings Merge

`lib/sandbox/linux/entrypoint.sh` gains a merge step after the existing `/workspace/.claude/settings.json` block: if `/workspace/.wrapix/ralph/runtime/<label>/claude-settings.json` exists (path resolved via a `RALPH_RUNTIME_DIR` env var exported by ralph on the host), its `hooks` tree is deep-merged into `~/.claude/settings.json`. This keeps ralph's per-invocation hook config separate from user customizations in `settings.local.json` and from the sandbox-wide `baseClaudeSettings.hooks` defined in `lib/sandbox/default.nix`.

The merge concatenates arrays under each hook event rather than replacing them, so ralph's `SessionStart[compact]` coexists with the existing `Notification` hook and any user-configured hooks.

### Lifecycle

1. Host-side shell computes the re-pin content from the command's variables and writes both files under `.wrapix/ralph/runtime/<label>/`.
2. Host invokes wrapix with `--env RALPH_RUNTIME_DIR=.wrapix/ralph/runtime/<label>` (wrapix scrubs host env by default; explicit `--env` is required). The runtime directory itself reaches the container through the existing `/workspace` bind mount — no extra mount is needed, but this dependency on the bind mount is a precondition of the feature.
3. Container entrypoint reads the env var, merges the settings fragment into `~/.claude/settings.json`, and starts Claude.
4. On auto-compaction, the `SessionStart` hook fires and the re-pin re-enters context.
5. When the container exits, the host-side shell removes `.wrapix/ralph/runtime/<label>/`.

The runtime directory is not committed to git; `.wrapix/ralph/runtime/` is added to `.gitignore`.

### Helpers

`lib/ralph/cmd/util.sh` gains two helpers:

- `build_repin_content <label> <command> [key=value ...]` — composes the condensed re-pin string from known keys (spec path, molecule ID, issue ID, companion paths, exit signals for the command).
- `install_repin_hook <label> <content>` — writes `repin.sh` (chmod +x) and `claude-settings.json` into `.wrapix/ralph/runtime/<label>/` and exports `RALPH_RUNTIME_DIR`. Called by each container-executed command before `run_claude_stream`.

Cleanup of the runtime directory is handled by a trap in the command script so it runs on both success and failure paths.

## Spec File Format

```markdown
# Feature Name

Overview of the feature.

## Problem Statement

Why this feature is needed.

## Requirements

### Functional
1. Requirement one
2. Requirement two

### Non-Functional
1. Performance requirement

## Affected Files

| File | Role |
|------|------|
| `path/to/file.nix` | Description |

## Success Criteria

- [ ] Criterion one
- [ ] Criterion two

## Out of Scope

- Thing not included
```

## Template System

### Nix-Native Templates

Templates are defined as Nix expressions with static validation:

```nix
# lib/ralph/template/default.nix
{ lib }:
let
  mkTemplate = { body, partials ? [], variables }:
    let
      resolvedPartials = map (p: builtins.readFile p) partials;
      content = builtins.readFile body;
    in {
      inherit content variables partials;
      render = vars:
        assert lib.assertMsg
          (builtins.all (v: vars ? ${v}) variables)
          "Missing required variables: ${builtins.toJSON variables}";
        lib.replaceStrings
          (map (v: "{{${v}}}") variables)
          (map (v: vars.${v}) variables)
          content;
    };
in {
  plan-new = mkTemplate {
    body = ./plan-new.md;
    partials = [ ./partial/context-pinning.md ./partial/exit-signals.md ];
    variables = [ "PINNED_CONTEXT" "LABEL" "SPEC_PATH" ];
  };
  # ... other templates
}
```

### Partials

Shared content via `{{> partial-name}}` markers:

```markdown
## Instructions

{{> context-pinning}}

1. Read the spec...
```

Resolved during template rendering.

### Template Structure

```
lib/ralph/template/
├── default.nix              # Template definitions + validation
├── partial/
│   ├── companions-context.md # Companion manifest injection
│   ├── context-pinning.md   # Project context loading
│   ├── exit-signals.md      # Exit signal format
│   ├── interview-modes.md   # "one by one" / "polish the spec" fast phrases
│   └── spec-header.md       # Label, spec path block
├── check.md                 # Post-loop reviewer prompt (invariant-clash aware)
├── msg.md                   # Interactive Drafter session prompt for clarify resolution
├── plan-new.md              # New spec interview
├── plan-update.md           # Update existing spec
├── todo-new.md              # Create molecule
├── todo-update.md           # Bond new tasks
└── run.md                   # Single-issue implementation
```

### Template Variables

| Variable | Source | Used By |
|----------|--------|---------|
| `PINNED_CONTEXT` | Read from `pinnedContext` file | all |
| `LABEL` | From command args | all |
| `SPEC_PATH` | Computed from label + mode | all |
| `SPEC_CONTENT` | Read from spec file | todo-new, run |
| `EXISTING_SPEC` | Read from `specs/<label>.md` | plan-update, todo-update |
| `SPEC_DIFF` | From `git diff base_commit..HEAD` (tier 1) | todo-update |
| `EXISTING_TASKS` | From molecule task list (tier 2) | todo-update |
| `COMPANIONS` | From `read_manifests` (companion directories) | plan-update, todo-new, todo-update, run, check, msg |
| `CLARIFY_BEADS` | Outstanding `ralph:clarify` beads for the spec (ID, title, description) | msg |
| `IMPLEMENTATION_NOTES` | From `implementation_notes` in `state/<label>.json` | todo-new, todo-update |
| `MOLECULE_ID` | From `state/<label>.json` | todo-update, run |
| `ISSUE_ID` | From `bd ready` | run |
| `TITLE` | From issue | run |
| `DESCRIPTION` | From issue | run |
| `BEADS_SUMMARY` | From molecule (titles + status) | check |
| `BASE_COMMIT` | From `state/<label>.json` | check |
| `PREVIOUS_FAILURE` | From previous attempt error output | run (retry only) |
| `EXIT_SIGNALS` | Template-specific list | all (via partial) |

### Flake Check Integration

```nix
# flake.nix
{
  checks.${system} = {
    ralph-templates = ralph.lib.validateTemplates {
      templates = ./lib/ralph/template;
    };
  };
}
```

## Partial Content Requirements

**`partial/context-pinning.md`:**
```markdown
## Context Pinning

First, read the project overview to understand project terminology:

{{PINNED_CONTEXT}}
```

**`partial/exit-signals.md`:**
```markdown
## Exit Signals

Output ONE of these at the end of your response:

{{EXIT_SIGNALS}}
```

**`partial/companions-context.md`:**
```markdown
{{COMPANIONS}}
```

Ships with just `{{COMPANIONS}}`; overridable by local template overlay.

**`partial/spec-header.md`:**
```markdown
## Current Feature

Label: {{LABEL}}
Spec file: {{SPEC_PATH}}
```

**`partial/interview-modes.md`:**
```markdown
## Interview Modes

The user may request one of these structured sub-modes at any point in the interview. Phrase matching is loose — respond to intent, not exact wording.

- **"one by one"** (also: "let's go through one by one", "go through them one at a time", etc.) — When you have multiple open design questions, present them individually in sequence. For each question, propose a suggested default with a short rationale, and wait for the user to accept, reject, or adjust before moving to the next. Optimizes for user attention: small decisions per turn, defaults ready to rubber-stamp.

- **"polish the spec"** (also: "polish this spec", "give it a polish", "do a polish pass", etc.) — Read the full spec end-to-end and report on: readability issues (unclear phrasing, missing context), consistency issues (contradictions between sections, terminology drift), ambiguities (statements that could be read multiple ways), and structural problems (misplaced content, missing sections). Propose specific edits for each finding. Typically run at end of a planning session, but available at any point.

Both modes remain within planning-only scope — no code changes, only spec edits.
```

Ships with the above content; overridable by local template overlay.

## State Management

### Per-Label State Files

Each workflow has its own state file at `state/<label>.json`:

**`state/<label>.json`:**
```json
{
  "label": "my-feature",
  "spec_path": "specs/my-feature.md",
  "molecule": "wx-9mvh",
  "base_commit": "abc123def456...",
  "companions": ["specs/e2e", "docs/api"]
}
```

| Field | Description |
|-------|-------------|
| `label` | Feature identifier (required) |
| `spec_path` | Full path to spec file (required) |
| `molecule` | Beads molecule ID (set by `ralph todo`) |
| `base_commit` | Git commit SHA at which spec was last fully tasked (set by `ralph todo` on success) |
| `companions` | Array of repo-relative directory paths containing `manifest.md` files |
| `implementation_notes` | Array of strings — transient implementation hints for task creation (optional; cleared by `ralph todo` on `RALPH_COMPLETE`) |
| `iteration_count` | Integer — current position in the `run ↔ check` auto-iteration (optional; reset on clean RALPH_COMPLETE push or clarify clear via `ralph msg`) |

### Active Workflow Pointer

**`state/current`** (plain text, no extension) holds the active label name:
```
my-feature
```

This file is the default when no `--spec` flag is given. Set by `ralph plan` and `ralph use`.

### Spec Label Resolution

Commands that accept `--spec/-s` resolve the target workflow as follows:
1. If `--spec <name>` provided → use `state/<name>.json`
2. If no `--spec` → read label from `state/current` → use `state/<label>.json`
3. If `state/current` does not exist and no `--spec` given → error with clear message

Commands with `--spec` support: `ralph todo`, `ralph run`, `ralph status`, `ralph logs`, `ralph check`, `ralph msg`
Commands without `--spec`: `ralph plan` (takes label as positional arg), `ralph tune`, `ralph sync`, `ralph use` (takes label as positional arg), `ralph init`

### Anchor & Sibling State Files

Under the anchor-driven multi-spec planning model (see [ralph-loop.md](ralph-loop.md)), the anchor's state file (`state/<label>.json` where `<label>` is the anchor) owns the session: it holds the molecule ID, `implementation_notes`, and `iteration_count`. Sibling specs touched during an anchor session do **not** get their own molecule — they share the anchor's.

What sibling state files DO hold:
- `label`, `spec_path`, `base_commit`, `companions`

What they do NOT hold (these are anchor-only):
- `molecule`, `implementation_notes`, `iteration_count`

Sibling state files are created lazily by `ralph todo` at RALPH_COMPLETE time (only for siblings that received tasks). A sibling that's later planned as its own anchor can set its own `molecule` at that point — its `base_commit` (already advanced during the earlier sibling session) prevents task duplication.

### Backwards Compatibility

Serial workflow is unchanged: `ralph plan` sets `state/current`, subsequent commands pick it up automatically. The only structural difference is the state file location (`state/<label>.json` + `state/current` vs the old singleton `state/current.json`).

Single-spec `ralph plan -u <label>` continues to work unchanged — the spec is simply its own anchor with an empty sibling set. The fan-out machinery reduces to a single-file diff.

Existing specs without `base_commit` are handled via four-tier fallback:
- If molecule in state JSON → tier 2 (LLM compares spec against tasks)
- If no state file but molecule in README → tier 3 (reconstruct state, then tier 2)
- If no molecule anywhere → tier 4 (full spec decomposition, creates new molecule)

After first successful `ralph todo`, `base_commit` is set and fast-path diffing works going forward.

## Project Configuration

Projects configure ralph via `.wrapix/ralph/config.nix` (local project overrides):

```nix
# .wrapix/ralph/config.nix
{
  # Wrapix flake reference (provides container profiles)
  wrapix = "github:user/wrapix";  # or local path

  # Context pinning - file read for {{PINNED_CONTEXT}}
  pinnedContext = "docs/README.md";

  # Spec locations
  specDir = ./specs;
  stateDir = ./state;

  # Template overlay (optional, for local customizations)
  templateDir = ./.wrapix/ralph/template;  # null = use packaged only

  # Run loop settings
  loop = {
    max-retries = 2;      # per bead, retry with PREVIOUS_FAILURE context
    max-iterations = 3;   # per molecule, bounds run ↔ check auto-iteration before escalating via ralph:clarify
  };
}
```

**Defaults** (when no config exists):
```nix
{
  wrapix = null;            # Error if container commands need it
  pinnedContext = "docs/README.md";
  specDir = ./specs;
  stateDir = ./state;
  templateDir = null;       # Use packaged templates only
}
```

**Template loading order:**
1. Check `templateDir` (local overlay) first
2. Fall back to packaged templates

**Profile resolution:**
- Profiles (base, rust, python, debug) are defined in wrapix (see profiles.md spec)
- Ralph references profiles by name; wrapix provides the actual container configuration

## Integration with Beads Molecules

Ralph uses `bd mol` for work tracking:

- **Specs are NOT molecules** — Specs are persistent markdown; molecules are work batches
- **Each `ralph todo` creates/updates a molecule** — Epic becomes molecule root
- **Update mode bonds to existing molecules** — New tasks attach to prior work
- **Molecule ID stored in `state/<label>.json`** — Enables `ralph status` convenience wrapper

**Key molecule commands used by Ralph:**

| Command | Used by | Purpose |
|---------|---------|---------|
| `bd create --type=epic` | `ralph todo` | Create molecule root |
| `bd mol progress` | `ralph status` | Show completion % |
| `bd mol current` | `ralph status` | Show position in DAG |
| `bd mol bond` | `ralph run` | Attach discovered work |
| `bd mol stale` | `ralph status` | Warn about orphaned molecules |

**Not used by Ralph** (user calls directly):
- `bd mol squash` — User decides when to archive
- `bd mol burn` — User decides when to abandon

## Affected Files

| File | Role |
|------|------|
| `lib/ralph/cmd/ralph.sh` | Main dispatcher |
| `lib/ralph/cmd/status.sh` | Progress display |
| `lib/ralph/cmd/logs.sh` | Error-focused log viewer |
| `lib/ralph/cmd/tune.sh` | Template editing (interactive + integration) |
| `lib/ralph/cmd/sync.sh` | Template sync from packaged (includes --diff); calls shared `scaffold_docs`/`scaffold_agents`/`scaffold_templates` from util.sh |
| `lib/ralph/cmd/use.sh` | Active workflow switching with validation |
| `lib/ralph/cmd/init.sh` | Project bootstrap — flake.nix, .envrc, .gitignore, .pre-commit-config.yaml, bd init, CLAUDE.md symlink; calls shared scaffold functions |
| `lib/ralph/cmd/util.sh` | Shared helper functions (includes `resolve_spec_label`, `read_manifests`, `compute_spec_diff`, `discover_molecule_from_readme`, `ralph:clarify` label management, `build_repin_content`, `install_repin_hook`, `scaffold_docs`, `scaffold_agents`, `scaffold_templates`, `bootstrap_flake`, `bootstrap_envrc`, `bootstrap_gitignore`, `bootstrap_precommit`, `bootstrap_beads`, `bootstrap_claude_symlink`) |
| `lib/ralph/template/default.nix` | Nix template definitions |
| `lib/ralph/template/partial/companions-context.md` | Companion manifest injection partial |
| `lib/ralph/template/partial/context-pinning.md` | Project context partial |
| `lib/ralph/template/partial/exit-signals.md` | Exit signal format partial |
| `lib/ralph/template/partial/interview-modes.md` | Documents "one by one" and "polish the spec" fast phrases |
| `lib/ralph/template/partial/spec-header.md` | Label + spec path header partial |
| `lib/ralph/template/flake.nix` | Project flake template rendered by `bootstrap_flake` (flake-parts, apps.sandbox, devShell, treefmt) |
| `lib/ralph/template/pre-commit-config.yaml` | Pre-commit template rendered by `bootstrap_precommit` (treefmt pre-commit + nix flake check pre-push) |
| `lib/sandbox/linux/entrypoint.sh` | Merge ralph runtime `claude-settings.json` from `$RALPH_RUNTIME_DIR` into `~/.claude/settings.json` when the env var is set |
| `flake.nix` (top-level wrapix) | Expose `apps.init`; add `shellHook` passthru on `packages.${system}.ralph` |
| `.gitignore` (wrapix repo) | Exclude `.wrapix/ralph/runtime/` (runtime cleanup, not an `init` artifact — init's `.gitignore` edits happen in downstream projects) |

## Success Criteria

### State management

- [ ] `state/current.json` replaced by per-label `state/<label>.json` files
- [ ] `state/current` (plain text) tracks the active label
- [ ] `ralph use <name>` switches active label with validation
- [ ] `ralph todo --spec <name>` operates on the named workflow
- [ ] `ralph run --spec <name>` operates on the named workflow
- [ ] `ralph run` reads spec once at startup and is unaffected by later `ralph use`
- [ ] `ralph status --all` shows summary of all active workflows
- [ ] `ralph status --spec <name>` shows specific workflow status
- [ ] `ralph logs --spec <name>` shows specific workflow logs
- [ ] Serial workflow (no `--spec` flag) continues to work as before
- [ ] Clear error messages when `state/current` is missing and no `--spec` given
- [ ] `state/<label>.json` no longer contains `update` or `hidden` fields
  [verify](../tests/ralph/run-tests.sh#test_state_json_schema)

### Templates

- [ ] `ralph check -t` validates all templates and partials
  [verify](../tests/ralph/run-tests.sh#test_check_valid_templates)
- [ ] `nix flake check` includes template validation
  [verify](../tests/ralph/run-tests.sh#test_check_exit_codes)
- [ ] Templates use Nix-native definitions with static validation
  [verify](../tests/ralph/run-tests.sh#test_render_template_basic)
- [ ] Partials work via `{{> partial-name}}` markers
  [verify](../tests/ralph/run-tests.sh#test_plan_template_with_partials)
- [ ] `ralph tune` (interactive) identifies correct template and makes edits
  [judge](../tests/judges/ralph-workflow.sh#test_tune_interactive)
- [ ] `ralph tune` (integration) ingests diff and interviews about changes
  [judge](../tests/judges/ralph-workflow.sh#test_tune_integration)
- [ ] `ralph sync --diff` shows local template changes vs packaged
  [verify](../tests/ralph/run-tests.sh#test_diff_local_modifications)
- [ ] `ralph sync` updates templates and backs up customizations
  [verify](../tests/ralph/run-tests.sh#test_sync_backup)
- [ ] `ralph sync --dry-run` previews without executing
  [verify](../tests/ralph/run-tests.sh#test_sync_dry_run)

### Compaction re-pin

- [ ] `ralph plan`, `todo`, `run --once`, and `check` each write `repin.sh` and `claude-settings.json` to `.wrapix/ralph/runtime/<label>/` before launching the container
  [verify](../tests/ralph/run-tests.sh#test_repin_hook_files_written)
- [ ] Settings fragment registers a `SessionStart` hook with matcher `"compact"` pointing at `repin.sh`
  [verify](../tests/ralph/run-tests.sh#test_repin_hook_settings_shape)
- [ ] `repin.sh` emits `hookSpecificOutput.additionalContext` JSON with the condensed re-pin
  [verify](../tests/ralph/run-tests.sh#test_repin_script_output)
- [ ] Re-pin content excludes the full spec body, companion manifest bodies, and full issue description
  [verify](../tests/ralph/run-tests.sh#test_repin_content_is_condensed)
- [ ] Re-pin content stays under 10KB
  [verify](../tests/ralph/run-tests.sh#test_repin_content_size)
- [ ] `lib/sandbox/linux/entrypoint.sh` merges `$RALPH_RUNTIME_DIR/claude-settings.json` into `~/.claude/settings.json` when the env var is set and the file exists
  [verify](../tests/ralph/run-tests.sh#test_entrypoint_merges_ralph_settings)
- [ ] Merge concatenates hook arrays rather than replacing (ralph `SessionStart[compact]` coexists with sandbox `Notification` hook)
  [verify](../tests/ralph/run-tests.sh#test_entrypoint_merge_concatenates_hooks)
- [ ] Runtime directory is removed by a trap on both success and failure paths
  [verify](../tests/ralph/run-tests.sh#test_runtime_dir_cleanup)
- [ ] `.wrapix/ralph/runtime/` is listed in `.gitignore`
  [verify](../tests/ralph/run-tests.sh#test_runtime_dir_gitignored)
- [ ] Host-side commands (`status`, `logs`, `check -t`, `msg`, `tune`, `sync`, `use`) do not create a runtime directory or register the hook
  [verify](../tests/ralph/run-tests.sh#test_host_commands_no_repin_hook)
- [ ] Compacted session receives the re-pin on next model turn
  [judge](../tests/judges/ralph-workflow.sh#test_repin_after_compaction)
- [ ] Container-executed ralph commands pass `--env RALPH_RUNTIME_DIR=…` to wrapix; entrypoint only merges the settings fragment when the env var is set
  [verify](../tests/ralph/run-tests.sh#test_runtime_dir_env_propagation)

### Init

- [ ] `ralph init` runs on host, not in a container
  [verify](../tests/ralph/run-tests.sh#test_init_host_execution)
- [ ] `nix run github:taheris/wrapix#init` invokes `ralph init` in cwd
  [verify](../tests/ralph/run-tests.sh#test_init_flake_app)
- [ ] `ralph init` creates `flake.nix` from template when absent, skips when present
  [verify](../tests/ralph/run-tests.sh#test_init_flake_skip_existing)
- [ ] Generated `flake.nix` uses flake-parts, exposes `apps.sandbox`, `devShells.default` composing `${ralph.shellHook}`, `treefmt`, `checks.treefmt`
  [verify](../tests/ralph/run-tests.sh#test_init_flake_structure)
- [ ] Generated `flake.nix` systems list is `[x86_64-linux aarch64-linux aarch64-darwin]` (no x86_64-darwin)
  [verify](../tests/ralph/run-tests.sh#test_init_flake_systems)
- [ ] Generated `flake.nix` treefmt programs are deadnix, nixfmt, shellcheck, statix (no rustfmt)
  [verify](../tests/ralph/run-tests.sh#test_init_treefmt_programs)
- [ ] Generated `flake.nix` evaluates cleanly under `nix flake check` in a fresh directory
  [verify](../tests/ralph/run-tests.sh#test_init_flake_evaluates)
- [ ] `ralph init` creates `.envrc` with `use flake` content, skip-if-exists
  [verify](../tests/ralph/run-tests.sh#test_init_creates_envrc)
- [ ] `ralph init` appends missing `.gitignore` entries (`.direnv/`, `.wrapix/`, `result`, `result-*`) idempotently
  [verify](../tests/ralph/run-tests.sh#test_init_gitignore_idempotent)
- [ ] `ralph init` creates `.pre-commit-config.yaml` with `treefmt` pre-commit + `nix flake check` pre-push stages
  [verify](../tests/ralph/run-tests.sh#test_init_precommit_stages)
- [ ] `ralph init` runs `bd init` when `.beads/` is absent, skips otherwise
  [verify](../tests/ralph/run-tests.sh#test_init_bd_init_idempotent)
- [ ] `ralph init` creates `docs/README.md`, `docs/architecture.md`, `docs/style-guidelines.md` via shared `scaffold_docs`
  [verify](../tests/ralph/run-tests.sh#test_init_scaffolds_docs)
- [ ] `ralph init` creates `AGENTS.md` via shared `scaffold_agents`
  [verify](../tests/ralph/run-tests.sh#test_init_creates_agents)
- [ ] `ralph init` creates `CLAUDE.md` as symlink to `AGENTS.md` when absent, skips when `CLAUDE.md` exists (file or symlink)
  [verify](../tests/ralph/run-tests.sh#test_init_claude_symlink)
- [ ] `ralph init` populates `.wrapix/ralph/template/` via shared `scaffold_templates`
  [verify](../tests/ralph/run-tests.sh#test_init_scaffolds_templates)
- [ ] `ralph init` and `ralph sync` share `scaffold_docs`, `scaffold_agents`, `scaffold_templates` (one code path)
  [verify](../tests/ralph/run-tests.sh#test_scaffold_shared_code_path)
- [ ] `ralph init` prints per-artifact created/skipped summary + fixed 2-step next-steps block
  [judge](../tests/judges/ralph-workflow.sh#test_init_output_format)
- [ ] Top-level wrapix flake exposes `apps.init`
  [verify](../tests/ralph/run-tests.sh#test_wrapix_flake_exposes_init)
- [ ] Top-level wrapix flake's `packages.${system}.ralph` exposes a `shellHook` passthru
  [verify](../tests/ralph/run-tests.sh#test_wrapix_ralph_shellhook_passthru)

### Top-level diagram

- [ ] Workflow Phases diagram in the spec reflects `plan → todo → run → check → (done + push)`

## Out of Scope

- Cross-workflow file conflicts (user's responsibility to pick non-overlapping features)
- Workflow locking or mutual exclusion
- Limiting the number of concurrent workflows
- Automated testing integration
- PR creation automation
- Formula-based workflows (Ralph uses specs, not formulas)
- Cross-repo automation for template propagation (manual diff + tune)

### `ralph init`

- Git operations (`git init`, initial commit, remote setup) — user-owned
- Interactive prompts — init is non-interactive
- Path argument — operates on cwd only (flake-app form has no shell context to pass a path from)
- Overwrite/merge of existing `flake.nix` — skip-and-continue policy; user merges manually if retrofitting
- `mkCity` wiring in generated `flake.nix` — user opt-in; default flake keeps scope minimal
