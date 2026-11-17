# Execution lifecycle acceptance

`checks.<system>.execution-fixtures` runs the Rust conformance tests against
actual packaged shell fixtures on Darwin and Linux. These checks do not launch
containers or prove live journal completion.

`test-ci --json test-security-execution-metadata-lifecycle` owns live packaged
launches. On Darwin its shell entrypoint runs Rust integration tests; on Linux
it still runs the original Python harness. The Python source remains necessary
until equivalent Linux live acceptance is established. The native Linux build
and fixture checks are not substitutes for that evidence.

Both live implementations exercise Run and Spawn: normal exits 0/37, observed
agent/image startup mismatch, unknown completion after killing the host launcher
and independently removing the container, and concurrent Claude/Pi launches
sharing unrelated transcripts. They use the actual packaged launcher, generated
profiles and container CLI. No fake runtime supplies these live verdicts.

The Rust harness retains each process-group leader unreaped until cleanup, uses
separate output files to avoid pipe deadlock, and preserves the original
120-second launch/probe and 30-second cleanup/runtime-command deadlines. Darwin
refuses signals to zombie-only groups with EPERM; the harness accepts only an
observed all-zombie state for the exact still-held group, then reaps its leader.
Other signaling or observation errors fail. Independent named-container cleanup
is attempted after process cleanup and after assertions fail.

The initial original Darwin attempt failed before any tests because its direct
profile reference omitted `.image`. Correcting that reference allowed all four
methods to run: three passed, while killed-launcher cleanup reported Darwin
zombie-group EPERM in both modes. These are retained failures, not successful
original acceptance. The native live campaign passed all four scenarios plus
three harness controls. Linux keeps the corrected original reference and its
existing owner; no Linux live pass or full Python retirement is claimed.
