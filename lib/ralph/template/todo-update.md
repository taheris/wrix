# Add Tasks to Existing Molecule

You are adding new tasks to an existing molecule. Your job is to identify new or
changed requirements and create tasks ONLY for those changes.

{{> context-pinning}}

{{> spec-header}}

{{EXISTING_SPEC}}

{{> companions-context}}

{{IMPLEMENTATION_NOTES}}

## Spec Changes

If SPEC_DIFF is provided, use that to identify new requirements. Otherwise, use
EXISTING_TASKS to compare against the spec and identify gaps.

### Spec Diff (git-based)

```diff
{{SPEC_DIFF}}
```

### Existing Tasks

{{EXISTING_TASKS}}

## Existing Molecule

Molecule ID: {{MOLECULE_ID}}

Use `bd mol show {{MOLECULE_ID}}` to see the current tasks in this molecule.

## Instructions

1. **Identify changes** — If SPEC_DIFF is non-empty, focus on the added/changed lines
   in the diff. If SPEC_DIFF is empty, compare the full spec against EXISTING_TASKS
   to find requirements that lack corresponding tasks.
2. **Create new tasks as children of the molecule**:
   ```bash
   TASK_ID=$(bd create --title="<task title>" --description="<detailed description>" \
     --type=task --labels="spec-{{LABEL}},profile:<profile>" --parent="{{MOLECULE_ID}}" --silent)
   ```
3. **Assign profile per-task** based on implementation needs:
   - Tasks touching `.rs` files or using cargo → `profile:rust`
   - Tasks touching `.py` files or using pytest/pip → `profile:python`
   - Tasks touching only `.nix`, `.sh`, `.md` files → `profile:base`
4. **Set execution order** with `bd dep add` if new tasks depend on existing ones:
   ```bash
   bd dep add <new-task> <existing-task>  # new-task waits for existing-task
   ```
5. **Do NOT create tasks** for requirements that already have corresponding tasks
   in the molecule

### Key Concepts

| Mechanism | Purpose | Effect |
|-----------|---------|--------|
| `--parent` | Links task to molecule | Enables `ralph status` progress tracking |
| `bd dep add` | Sets execution order | Controls what `bd ready` returns next |
| `profile:X` | Selects container profile | Determines toolchain available in `ralph run` |

## Task Breakdown Guidelines

- Each task should be **self-contained** with enough context for a fresh agent
- Consider dependencies on **existing tasks** in the molecule
- Keep tasks **focused** - one clear objective per task
- Include **test tasks** where appropriate
- **Assign profile per-task** based on what that specific task needs

## README Backfill

After creating tasks, check if `specs/README.md` has an empty Beads column for this spec (`{{LABEL}}`).
If the Beads column is empty, fill in the molecule ID (`{{MOLECULE_ID}}`).

{{> exit-signals}}

- `RALPH_COMPLETE` - New tasks created and dependencies set
- `RALPH_BLOCKED: <reason>` - Cannot proceed (molecule not found, unclear requirements)
