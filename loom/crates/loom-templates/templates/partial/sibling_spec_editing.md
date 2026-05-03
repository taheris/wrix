## Anchor Session & Sibling-Spec Editing

The label named on the `-u` flag (`{{ label }}`) is the **anchor**; it owns the
session state row in the loom state database. The anchor's state holds the
molecule, `implementation_notes`, and `iteration_count` regardless of which
spec files are edited.

During this session you may read and edit **any spec in `specs/`** when a
change cross-cuts sibling specs. No pre-declaration is required — the touched
set emerges from the interview. `docs/README.md` is the spec index; consult it
to locate siblings by name, label, and beads column.

Rules:

- Edit sibling specs in place under `specs/` using the Edit tool, just like the
  anchor
- Sibling specs do **not** get their own state row or molecule during this
  session; `loom todo` creates sibling state rows lazily on fan-out
