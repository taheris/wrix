# Beads Issue Tracking

Lightweight issue tracker with first-class dependency support for AI agent workflows.

## Problem Statement

AI coding agents need persistent issue tracking that:
- Survives across sessions and context windows
- Tracks dependencies between tasks
- Syncs between host and container environments
- Integrates with git workflows
- Provides a CLI interface suitable for agent use

## Requirements

### Functional

1. **Issue CRUD** - Create, read, update, delete issues via `bd` CLI
2. **Dependencies** - First-class support for blocking relationships
3. **Ready Queue** - `bd ready` shows unblocked work
4. **Status Tracking** - Issues move through open → in_progress → closed
5. **Priority Levels** - P0 (critical) through P4 (backlog)
6. **Issue Types** - task, bug, feature, epic, question, docs
7. **Sync** - `bd dolt push` / `bd dolt pull` sync via Dolt remotes

### Non-Functional

1. **Agent-Friendly** - CLI designed for AI agent consumption
2. **Portable** - Works in containers via mounted `.beads/` directory
3. **Conflict-Free** - Dolt-native sync handles concurrent edits

## CLI Commands

| Command | Purpose |
|---------|---------|
| `bd ready` | Show issues ready to work (no blockers) |
| `bd list --status=open` | List all open issues |
| `bd show <id>` | Show issue details with dependencies |
| `bd create --title="..." --type=task --priority=2` | Create issue |
| `bd update <id> --status=in_progress` | Update issue |
| `bd close <id>` | Close issue |
| `bd dep add <issue> <depends-on>` | Add dependency |
| `bd dolt pull` | Pull from Dolt remote |
| `bd dolt push` | Push to Dolt remote |

## Storage

```
.beads/
├── config.yaml      # Repository configuration
├── metadata.json    # Database metadata
├── dolt/            # Dolt database (primary storage)
└── dolt-remote/     # Dolt remote for container sync
```

### Sync

Dolt-native sync via `bd dolt pull` / `bd dolt push`. Requires `dolt sql-server` running
on port 3307 (auto-started by devShell). The Dolt remote lives in the beads branch worktree.

## Workflow Integration

### Agent Session Pattern

```bash
bd dolt pull                         # Pull latest
bd ready                             # Find work
bd update <id> --status=in_progress  # Claim
# ... do work ...
bd close <id>                        # Complete
bd dolt push                         # Push changes
```

### Ralph Integration

Ralph uses beads for issue tracking:
- `ralph todo` creates issues from specs via `bd create`
- `ralph run` finds work via `bd ready`
- Issues link to specs via description field

## Configuration

Key settings in `.beads/config.yaml`:

| Setting | Purpose |
|---------|---------|
| `issue-prefix` | Prefix for issue IDs (e.g., "wx" → "wx-1") |
| `sync-branch` | Git branch for beads data |
| `sync.mode` | Sync mode: `dolt-native` |
| `federation.remote` | Dolt remote URL for container sync |

## Affected Files

| File | Role |
|------|------|
| `.beads/config.yaml` | Repository configuration |
| `.beads/dolt/` | Dolt database storage |
| `CLAUDE.md` | Agent instructions for beads usage |

## Success Criteria

- [ ] Issues persist across agent sessions
  [system](bash tests/ralph/run-tests.sh test_isolated_beads_db)
- [ ] Dependencies correctly block `bd ready` output
  [system](bash tests/ralph/run-tests.sh test_run_respects_dependencies)
- [ ] `bd dolt pull`/`bd dolt push` works in container environment
  [judge](tests/judges/beads.sh::test_sync_in_container)
- [ ] Priority and status filtering works
  [system](bash tests/ralph/run-tests.sh test_config_data_driven)
- [ ] Issues can be created with descriptions
  [system](bash tests/ralph/run-tests.sh test_discovered_work)

## Out of Scope

- Beads CLI implementation (external tool)
- Web UI for issue management
- Integration with external trackers (Jira, Linear)
