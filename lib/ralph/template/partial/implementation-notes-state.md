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
