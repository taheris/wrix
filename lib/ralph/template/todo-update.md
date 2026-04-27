# Add Tasks to Existing Molecule

You are adding new tasks to an existing molecule. Your job is to identify new or
changed requirements and create tasks ONLY for those changes.

{{> context-pinning}}

{{> spec-header}}

## Anchor Spec

`{{LABEL}}` is the **anchor** for this session. The anchor spec below
(`specs/{{LABEL}}.md`) is shown for context — it's the spec that owns the
molecule, and it reflects what has already been implemented. Under
**Anchor-Driven Multi-Spec Planning**, a single `ralph todo` run may also
create tasks for sibling specs in `specs/` that were touched during the plan
session; those sibling diffs appear in the Spec Diff section below.

Read the anchor spec at `{{SPEC_PATH}}` for the full current contents before identifying changes.

{{> companions-context}}

{{IMPLEMENTATION_NOTES}}

## Spec Changes

If `SPEC_DIFF` is provided, use that to identify new requirements **across all
listed specs**; otherwise use `EXISTING_TASKS` to compare against the anchor
spec and identify gaps.

Under **per-spec cursor fan-out**, `SPEC_DIFF` may span multiple sibling specs.
Each diff block is prefixed with its spec path in the form `=== specs/<s>.md ===`
so you can attribute every new requirement to the correct spec file.

### Spec Diff (git-based)

```diff
{{SPEC_DIFF}}
```

### Existing Tasks

{{EXISTING_TASKS}}

## Existing Molecule

Molecule ID: {{MOLECULE_ID}}

Use `bd mol show {{MOLECULE_ID}}` to see the current tasks in this molecule.
All new tasks in this session — whether they implement the anchor or a sibling
spec — bond to this molecule. Sibling specs do NOT get their own molecule.

## Instructions

1. **Identify changes** — If `SPEC_DIFF` is non-empty, focus on the added/changed
   lines in the diff across every spec listed under its `=== specs/<s>.md ===`
   headers. If `SPEC_DIFF` is empty, compare the full anchor spec against
   `EXISTING_TASKS` to find requirements that lack corresponding tasks.
2. **Create new tasks as children of the anchor's molecule**, labelling each
   task with the spec it implements (may differ from the anchor under fan-out):
   ```bash
   # <s> is the spec file the task implements, e.g. the anchor ({{LABEL}}) or
   # a sibling label derived from specs/<sibling>.md.
   TASK_ID=$(bd create --title="<task title>" --description="<detailed description>" \
     --type=task --labels="spec:<s>,profile:<profile>" --parent="{{MOLECULE_ID}}" --silent)
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
| `--parent` | Links task to the anchor's molecule | Enables `ralph status` progress tracking |
| `bd dep add` | Sets execution order | Controls what `bd ready` returns next |
| `spec:<s>` | Marks which spec file a task implements | Attributes tasks to anchor vs sibling under fan-out |
| `profile:X` | Selects container profile | Determines toolchain available in `ralph run` |

## Task Breakdown Guidelines

- Each task should be **self-contained** with enough context for a fresh agent
- Consider dependencies on **existing tasks** in the molecule
- Keep tasks **focused** - one clear objective per task
- Include **test tasks** where appropriate
- **Assign profile per-task** based on what that specific task needs
- **Label each task with `spec:<s>`** identifying which spec file it implements
  — under fan-out, `<s>` may be the anchor label (`{{LABEL}}`) or a sibling label

## Spec Index Backfill (anchor only)

After creating tasks, check if the pinned-context file (the project spec index
shown above under "Context Pinning" — `docs/README.md` by default) has an empty
Beads column for the **anchor** spec (`{{LABEL}}`). If the Beads column is
empty, fill in the molecule ID (`{{MOLECULE_ID}}`).

Sibling specs touched in this session do **not** get their own molecule — their
tasks bond to the anchor's molecule. Do **NOT** write the anchor's molecule ID
into any sibling's README Beads column; a sibling's row stays empty until it is
planned as its own anchor in a future `ralph plan -u <sibling>` session.

{{> exit-signals}}

- `RALPH_COMPLETE` - New tasks created and dependencies set
- `RALPH_BLOCKED: <reason>` - Cannot proceed (molecule not found, unclear requirements)
