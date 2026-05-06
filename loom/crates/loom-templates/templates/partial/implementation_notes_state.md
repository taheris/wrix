## Implementation Notes

During the interview, you may gather implementation hints — specific technical
details that help the implementer but don't belong in the permanent spec
(e.g., "remove the rustup bootstrap block from entrypoint.sh", "use fenix's
fromToolchainFile").

Existing notes for `{{ label }}` (the merge basis):

{% if existing_implementation_notes.is_empty() %}_(no existing notes for this spec)_
{% else %}{% for note in existing_implementation_notes %}- {{ note }}
{% endfor %}{% endif %}

Merge those existing notes with anything new from this interview — keep notes
still relevant, drop ones a new decision invalidates, and add fresh ones. The
merge is your judgement, not a blind append or replace. Then write the merged
set as a `## Implementation Notes` section at the very end of the anchor spec
`specs/{{ label }}.md` — `loom plan` parses that section after the interview
exits and persists the array to the anchor's state-DB row. Sibling specs do
**not** carry implementation notes.

These notes are automatically passed to `loom todo` templates during task
creation and cleared from the row once consumed.
