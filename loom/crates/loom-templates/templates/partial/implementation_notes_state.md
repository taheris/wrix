## Implementation Notes

During the interview, you may gather implementation hints — specific technical details
that help the implementer but don't belong in the permanent spec (e.g., "remove the
rustup bootstrap block from entrypoint.sh", "use fenix's fromToolchainFile").

Store these in the **anchor's state DB row** for `{{ label }}` as an
`implementation_notes` array of strings. Implementation notes **always** live in the
anchor's state regardless of which sibling spec they apply to — sibling rows never
hold `implementation_notes`. Do NOT add an "Implementation Notes" section to any spec
markdown.

These notes are automatically passed to `loom todo` templates during task creation.
