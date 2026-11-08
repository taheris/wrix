# Locked Loom Consumer Measurement

This bounded trace separates evaluation, realization, environment generation and
activation for Loom's real `ci` development shell on aarch64-darwin. The Wrix
input is locked to published commit `c75c87a06614c244e2360fd8be9f92186bd3e302`.
The consumer is the uncommitted `lm-p6xb` migration checkout based on
`114b2e06`; this is not a measurement of a published Loom revision.

| Phase                        | Seconds | Outcome                        |
| ---------------------------- | ------: | ------------------------------ |
| Evaluate CI shell derivation |  21.048 | Passed                         |
| First realization attempt    | 900.043 | Bounded timeout                |
| Ordinary realization retry   | 456.510 | Passed, retained previous work |
| Generate environment         |   2.632 | Passed                         |
| First observed activation    |   7.004 | Passed                         |
| Cached activation            |   2.044 | Passed                         |

Commands, input-lock SHA-256 and qualifications are retained in
[`locked-consumer-measurement.json`](locked-consumer-measurement.json).
Realization built the Loom package and shell environment. The timeout and retry
are one interrupted campaign, not independent cold observations; their sum is
not a controlled compilation measurement.

## Scope and Safety

The CI shell is an existing real consumer output, not a synthetic unlocked
fixture. It has no Wrix service-start hook. Activation ran its normal Cargo
patch setup without starting or upgrading shared services. The interactive
default shell was not activated: its hook requests service startup, which this
measurement was explicitly prohibited from doing.

The common toolchain, vendor inputs and store were already populated, and
concurrent verification created contention. The consumer source was dirty,
although its Wrix input was locked. These observations establish phase
boundaries and a cached-reload observation, not a speedup, empty-store result,
interactive default-shell timing or proof that the Loom migration is ready to
land.
