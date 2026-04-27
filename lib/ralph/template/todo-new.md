# Task Decomposition

You are decomposing a specification into implementable tasks. Your goal is to
create a beads molecule (epic + child issues) that breaks down the work.

{{> context-pinning}}

{{> spec-header}}

## Specification Content

Read the spec at `{{SPEC_PATH}}` for full content before decomposing.

{{> companions-context}}

{{IMPLEMENTATION_NOTES}}

## Task Breakdown Guidelines

- Each task should be **self-contained** with enough context for a fresh agent
- Order tasks by **dependencies** (what must be done first)
- Keep tasks **focused** - one clear objective per task
- Include **test tasks** where appropriate
- Consider: setup, implementation, tests, documentation
- **Assign profile per-task** based on what that specific task needs

## Profile Assignment

Each task needs a `profile:X` label to select the right container toolchain in `ralph run`:

| Task Type | Profile | When to Use |
|-----------|---------|-------------|
| Rust implementation | `profile:rust` | Tasks touching `.rs` files or using cargo |
| Python implementation | `profile:python` | Tasks touching `.py` files or using pytest/pip |
| Nix/shell/docs | `profile:base` | Tasks touching only `.nix`, `.sh`, `.md` files |

Different tasks in the same molecule can have different profiles. Assign based on what each specific task needs.

## Instructions

1. **Analyze the spec** - Understand all requirements and affected files
2. **Create the epic** (molecule root) and **store its ID** in the state file:
   ```bash
   MOLECULE_ID=$(bd create --type=epic --title="<feature name>" --labels="spec:{{LABEL}}" --silent)
   echo "Created molecule: $MOLECULE_ID"
   jq --arg mol "$MOLECULE_ID" '.molecule = $mol' {{CURRENT_FILE}} > {{CURRENT_FILE}}.tmp && mv {{CURRENT_FILE}}.tmp {{CURRENT_FILE}}
   ```
   **CRITICAL:** Use the exact ID returned by `bd create --silent`. Do NOT substitute
   a molecule ID from the spec index or any other source — `bd create` generates a
   unique ID and that is the only valid value.
3. **Create child tasks** with `--parent` and appropriate `profile:X` label:
   ```bash
   TASK_ID=$(bd create --title="<task title>" --description="<detailed description>" \
     --type=task --labels="spec:{{LABEL}},profile:rust" --parent="$MOLECULE_ID" --silent)
   ```
4. **Set execution order** with `bd dep add` for tasks that must run sequentially:
   ```bash
   bd dep add <later-task> <earlier-task>  # later-task waits for earlier-task
   ```

### Key Concepts

| Mechanism | Purpose | Effect |
|-----------|---------|--------|
| `--parent` | Links task to molecule | Enables `ralph status` progress tracking |
| `bd dep add` | Sets execution order | Controls what `bd ready` returns next |
| `profile:X` | Selects container profile | Determines toolchain available in `ralph run` |

Both `--parent` and `bd dep add` are required: `--parent` for visibility, `bd dep add` for ordering.

## Output Format

After creating all tasks:

1. List the epic ID and all task IDs created
2. Show the dependency graph
3. Confirm the molecule was created

{{README_INSTRUCTIONS}}

{{> exit-signals}}

- `RALPH_COMPLETE` - All tasks created, dependencies set, molecule created
- `RALPH_BLOCKED: <reason>` - Cannot decompose spec (missing information, unclear requirements)
