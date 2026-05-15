## Exit Signals

End your response with exactly **one** of these markers on its own line, as
the final output of the session. The orchestrator parses the final line
verbatim to derive the gate's verdict; emit it with nothing trailing.

- `LOOM_COMPLETE` — The work succeeded. For worker phases (`loom run`), this
  also means the bead's acceptance criteria are met and the bead has been
  closed via `bd close`. The diff must be non-empty (real changes); see
  `LOOM_NOOP` below for the zero-diff variant.
- `LOOM_NOOP` — The work was already done in tree and this phase
  intentionally produced an empty diff. Close the bead with `bd close`
  before emitting. Use `LOOM_NOOP` instead of `LOOM_COMPLETE` whenever the
  diff is empty — an empty diff with `LOOM_COMPLETE` is treated as
  `zero-progress` and enters recovery. Only valid in worker phases.
- `LOOM_BLOCKED` — You cannot proceed and are self-reporting. Write the
  reason on prior lines before the marker. The gate applies `loom:blocked`
  to *this* bead and exits without entering recovery; other beads in the
  molecule continue running. The labelled bead waits for human resolution
  via `loom msg`.
- `LOOM_CLARIFY` — You have a specific question with structured options for
  the human (per the Options Format Contract in `specs/loom-gate.md`).
  Write the question and option block on prior lines before the marker.
  The gate applies `loom:clarify` to *this* bead and exits without
  entering recovery; other beads in the molecule continue running. The
  labelled bead waits for `loom msg` resolution.
