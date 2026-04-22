# Implementation Step

{{> context-pinning}}

{{> spec-header}}

{{> companions-context}}

## Current Spec

Read: {{SPEC_PATH}}

## Issue Details

Issue: {{ISSUE_ID}}
Title: {{TITLE}}

{{DESCRIPTION}}

{{PREVIOUS_FAILURE}}

## Instructions

1. **Understand**: Read the spec and issue thoroughly before making changes
2. **Test Strategy**: Decide between:
   - Property-based tests: For functions with clear invariants, mathematical properties
   - Unit tests: For specific behaviors, edge cases, integration points
3. **Implement**: Write code following the spec
4. **Discovered Work**: If you find tasks outside this issue's scope:
   - Create the issue as a child of the molecule:
     ```bash
     NEW_ID=$(bd create --title="..." --type=task --labels="spec:{{LABEL}}" \
       --parent="{{MOLECULE_ID}}" --silent)
     ```
   - Set execution order if needed:
     - **Blocks current task**: `bd dep add {{ISSUE_ID}} $NEW_ID` (current waits for new)
     - **Depends on current task**: `bd dep add $NEW_ID {{ISSUE_ID}}` (new waits for current)
     - **Independent**: No dep needed—`bd ready` will surface it when unblocked
   - Do NOT implement discovered tasks in this session—stay focused
5. **Quality Gates**: Before completing, ensure:
   - [ ] All tests pass
   - [ ] Lint checks pass
   - [ ] Changes committed
6. **Blocked vs Waiting**: Distinguish dependency blocks from true blocks:
   - Need user input? → `RALPH_BLOCKED: <reason>`
   - Need other beads done? → Add dep with `bd dep add`, output `RALPH_COMPLETE`
7. **Already Implemented**: If the task's work is already done in the codebase,
   verify correctness, close the issue, and move on with `RALPH_COMPLETE`

## Spec Verifications

After implementing the issue, check the spec's Success Criteria for `[verify]` and
`[judge]` annotations that reference test files relevant to this issue's work.

- **`[verify]` tests**: Create the referenced shell test file if it doesn't exist.
  The test function should exercise the implemented feature and exit 0 on success,
  non-zero on failure. Use `set -euo pipefail`. Exit 77 to skip (e.g. platform
  not available).
- **`[judge]` tests**: Create the referenced judge rubric file if it doesn't exist.
  Each function calls `judge_files "path/to/source"` and
  `judge_criterion "what to evaluate"`. See `tests/judges/example.sh` for format.
- Only create tests for criteria related to the current issue — don't implement
  all spec verifications, just the ones relevant to your work.
- If the test file already exists, add your function to it rather than overwriting.

## Quality Gates

Before outputting RALPH_COMPLETE:
- [ ] Tests written and passing
- [ ] Lint checks pass
- [ ] Changes staged (`git add`)
- [ ] Spec verification test files created for relevant criteria

Post-step hooks verify compliance automatically.

## Land the Plane

Before outputting RALPH_COMPLETE, follow the **Session Protocol** in `AGENTS.md`.

{{> exit-signals}}

- `RALPH_COMPLETE` - Task finished, all quality gates passed
- `RALPH_BLOCKED: <reason>` - Cannot proceed, explain why
- `RALPH_CLARIFY: <question>` - Need clarification before proceeding
