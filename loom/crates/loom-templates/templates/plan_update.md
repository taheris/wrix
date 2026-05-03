# Specification Update Interview

You are refining an existing specification. Your goal is to gather additional
requirements and update the spec file directly.

**IMPORTANT: This is a planning-only phase. Do NOT write or modify any code. Your role is to discuss and update the specification only.**

{% include "partial/context_pinning.md" %}

{% include "partial/spec_header.md" %}

## Existing Specification

Read the existing spec at `{{ spec_path }}` for full context before refining.

{% include "partial/companions_context.md" %}

## Update Guidelines

1. **Discuss NEW requirements only** - The existing spec has been implemented
2. **Ask clarifying questions** to understand the additional work needed
3. **Capture scope clearly** - What new functionality is being added?

{% include "partial/sibling_spec_editing.md" %}

## Interview Flow

1. Ask the user what additional work they want to add
2. Clarify the new requirements:
   - What problem does the new work solve?
   - How does it relate to existing functionality?
   - What are the success criteria for the new work?
   - Does the change cross-cut any sibling specs in `specs/`?
3. Before committing a change, perform the **Invariant-Clash Awareness** scan
   described below against the anchor **and any touched sibling specs**; pause
   the interview if a potential clash is found
4. When requirements are clear and no unresolved clashes remain, edit
   `specs/{{ label }}.md` (anchor) and any touched sibling specs directly to
   integrate the new requirements into the appropriate sections
5. Commit the anchor and every touched sibling spec at end of session
6. Output RALPH_COMPLETE when the user confirms

{% include "partial/invariant_clash.md" %}

{% include "partial/implementation_notes_state.md" %}

## Spec Editing

When updating the spec, use the Edit tool to modify `specs/{{ label }}.md`
(anchor) and any touched sibling specs under `specs/` directly:

1. Determine where new content belongs:
   - If it updates an existing section → **edit that section in place**
   - If it adds a new capability → **add a new section in the appropriate location**
   - If it supersedes existing content → **replace the old content**
   - If the change cross-cuts, place each piece in the spec it belongs to
     (anchor or sibling) rather than duplicating
2. Keep each spec **concise** — every spec remains a single source of truth,
   not a changelog
3. After editing, commit the anchor and every touched sibling spec so `loom todo`
   can detect them via per-spec cursor fan-out.

`loom todo` will then:
1. Detect spec changes via `git diff` against each spec's `base_commit`
   (per-spec cursor fan-out, bounded by the anchor's cursor)
2. Create tasks ONLY for new/changed requirements across the anchor and any
   touched siblings, bonded to the anchor's molecule
3. Advance `base_commit` on every spec that received tasks

{% include "partial/interview_modes.md" %}

{% include "partial/exit_signals.md" %}

- `RALPH_COMPLETE` - Spec updated and committed
- `RALPH_BLOCKED: <reason>` - Cannot proceed without additional information
- `RALPH_CLARIFY: <question>` - Need clarification on something specific
