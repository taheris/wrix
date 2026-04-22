# Agent Instructions

## Specifications

Before implementing features, consult `docs/README.md`:

- **Architecture first** — Read `docs/architecture.md` for system overview
- **Check specs before coding** — Each feature has a dedicated spec file in `specs/`
- **Terminology** — `docs/README.md` has a terminology index

## Building

```bash
nix develop          # Enter devShell
nix build            # Build sandbox
nix build .#wrapix-rust    # With Rust profile
nix build .#wrapix-python  # With Python profile
nix build .#wrapix-mcp     # With all MCP servers (tmux, playwright)
```

## Issue Tracking (Beads)

**Use `bd` for ALL issue tracking.** Do NOT use markdown TODOs or external trackers.

```bash
bd ready                          # Show unblocked work
bd show <id>                      # Issue details
bd create --title="..." --description="..." --type=task --priority=2
bd update <id> --status=in_progress   # Claim before starting
bd close <id>                     # Mark complete
bd dep add <issue> <depends-on>   # Add dependency
```

**Priority:** 0-4 (critical to backlog, default 2). **Types:** task, bug, feature, epic.

**Workflow:** `bd ready` → `bd update --status=in_progress` → implement → `bd close`

## Session Protocol

### Start

```bash
bd dolt pull
```

### End ("land the plane")

```bash
git add <files>
git commit -m "..."   # Hooks run: nixfmt, shellcheck, flake check, tests
git push
beads-push            # Sync beads branch: bd dolt commit + push + git push origin beads
```

Work is NOT complete until both pushes succeed. `beads-push` is required — `bd dolt
push` alone does not sync the `beads` git branch to GitHub.

## Hidden Specs

Files in `.wrapix/ralph/state/` are **hidden specs** managed by Ralph. **NEVER** copy or
commit them to `specs/`. The `no-hidden-specs` pre-commit hook blocks this, but
`--no-verify` bypasses it. If you need to reference a hidden spec, read it in place —
do not create a corresponding file under `specs/`.

## Code Style

Read `docs/style-guidelines.md` before writing or reviewing code — it contains
the authoritative, enforceable rules (prefixed SH-, NX-, DOC-, GIT-, TST-).

Hooks enforce formatting automatically (nixfmt, shellcheck).

**IMPORTANT:** Use `nix fmt` to format Nix files, NOT `nixfmt` directly.

```bash
nix fmt             # Format all Nix files (works outside devShell)
nix fmt flake.nix   # Format specific file
```

The `nixfmt` command is only available inside `nix develop`.
