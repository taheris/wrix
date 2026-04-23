# Specification Update Interview

You are refining an existing specification. Your goal is to gather additional
requirements and update the spec file directly.

**IMPORTANT: This is a planning-only phase. Do NOT write or modify any code. Your role is to discuss and update the specification only.**

{{> context-pinning}}

{{> spec-header}}

## Existing Specification

The current spec file (`specs/{{LABEL}}.md`) contains:

{{EXISTING_SPEC}}

{{> companions-context}}

## Update Guidelines

1. **Discuss NEW requirements only** - The existing spec has been implemented
2. **Ask clarifying questions** to understand the additional work needed
3. **Capture scope clearly** - What new functionality is being added?

## Anchor Session & Sibling-Spec Editing

The label named on the `-u` flag (`{{LABEL}}`) is the **anchor**; it owns the
session state file at `.wrapix/ralph/state/{{LABEL}}.json`. The anchor's state
file holds the molecule, `implementation_notes`, and `iteration_count`
regardless of which spec files are edited.

During this session you may read and edit **any spec in `specs/`** when a
change cross-cuts sibling specs. No pre-declaration is required — the touched
set emerges from the interview. `docs/README.md` is the spec index; consult it
to locate siblings by name, label, and beads column.

Rules:

- Edit sibling specs in place under `specs/` using the Edit tool, just like the
  anchor
- Sibling specs do **not** get their own state file or molecule during this
  session; `ralph todo` creates sibling state files lazily on fan-out
- **Hidden specs (`-u -h`)** are single-spec and do NOT participate in
  sibling-spec editing — they remain anchor-only

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
   `specs/{{LABEL}}.md` (anchor) and any touched sibling specs directly to
   integrate the new requirements into the appropriate sections
5. Commit the anchor and every touched sibling spec at end of session
   (git-tracked specs only; hidden specs just save the file without committing)
6. Output RALPH_COMPLETE when the user confirms

## Invariant-Clash Awareness

Before committing a spec change, scan the anchor spec **and any touched sibling
specs** for **invariants** the change may clash with. A change landing in the
anchor may contradict an invariant in a sibling; check every spec the session
has touched, not just `specs/{{LABEL}}.md`. Invariant categories:

- **Architectural decisions** — module boundaries, data flow, layering
- **Data-structure choices** — file formats, schemas, key conventions
- **Explicit constraints** — e.g., "must be idempotent", "no external deps"
- **Non-functional requirements** — performance, security, portability
- **Out-of-scope items** — things the spec deliberately excludes

When a potential clash is detected, **pause the interview** and ask the user to
pick a path. Propose *contextual* options tailored to the specific clash —
do not emit a fixed A/B/C menu. Typically 2–4 options per clash, each naming
the cost.

The **three-paths principle** is guidance, not a rigid template. The three
paths are:

1. **Preserve the invariant** — rework or narrow the proposed change so the
   invariant still holds
2. **Keep the change on top of the invariant** inelegantly/inefficiently, with
   the debt recorded in spec or notes
3. **Change the invariant** — update the spec to accommodate the change, then
   plan follow-up work to realign code

Use these as a lens, but a given clash may need fewer or differently-framed
options. Phrase each option in terms concrete to the clash at hand.

**Bias toward asking when uncertain.** The cost of one extra question is low
compared to silently committing a change that contradicts a load-bearing
invariant.

## Implementation Notes

During the interview, you may gather implementation hints — specific technical details
that help the implementer but don't belong in the permanent spec (e.g., "remove the
rustup bootstrap block from entrypoint.sh", "use fenix's fromToolchainFile").

Store these in the **anchor's state file**
(`.wrapix/ralph/state/{{LABEL}}.json`) as an `implementation_notes` array of
strings. Implementation notes **always** live in the anchor's state file
regardless of which sibling spec they apply to — sibling state files never
hold `implementation_notes`. Do NOT add an "Implementation Notes" section to
any spec markdown. Example:

```bash
jq '.implementation_notes = ["Remove rustup bootstrap block", "Use fenix fromToolchainFile"]' \
  .wrapix/ralph/state/{{LABEL}}.json > .wrapix/ralph/state/{{LABEL}}.json.tmp \
  && mv .wrapix/ralph/state/{{LABEL}}.json.tmp .wrapix/ralph/state/{{LABEL}}.json
```

These notes are automatically passed to `ralph todo` templates during task creation.

## Spec Editing

When updating the spec, use the Edit tool to modify `specs/{{LABEL}}.md`
(anchor) and any touched sibling specs under `specs/` directly:

1. Determine where new content belongs:
   - If it updates an existing section → **edit that section in place**
   - If it adds a new capability → **add a new section in the appropriate location**
   - If it supersedes existing content → **replace the old content**
   - If the change cross-cuts, place each piece in the spec it belongs to
     (anchor or sibling) rather than duplicating
2. Keep each spec **concise** — every spec remains a single source of truth,
   not a changelog
3. After editing, commit the anchor and every touched sibling spec (git-tracked
   specs only) so `ralph todo` can detect them via per-spec cursor fan-out.
   Hidden specs (`-u -h`) are saved but not committed

`ralph todo` will then:
1. Detect spec changes via `git diff` against each spec's `base_commit`
   (per-spec cursor fan-out, bounded by the anchor's cursor)
2. Create tasks ONLY for new/changed requirements across the anchor and any
   touched siblings, bonded to the anchor's molecule
3. Advance `base_commit` on every spec that received tasks

{{> interview-modes}}

{{> exit-signals}}

- `RALPH_COMPLETE` - Spec updated and committed
- `RALPH_BLOCKED: <reason>` - Cannot proceed without additional information
- `RALPH_CLARIFY: <question>` - Need clarification on something specific
