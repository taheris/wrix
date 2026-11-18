# Execution lifecycle acceptance

`checks.<system>.execution-fixtures` runs Rust conformance against actual
packaged shell fixtures on Darwin and Linux. These checks do not launch
containers or prove live journal completion.

`test-ci --json test-security-execution-metadata-lifecycle` owns live packaged
launches. Both platforms execute the Nix-built Rust integration-test binary; the
source-bound fixture resources remain in its runtime closure. The owner does not
require an ambient Cargo build or Python harness. Each invocation runs all four
live scenarios, a release-gate regression and the three infrastructure controls
independently.

The live scenarios exercise Run and Spawn: normal exits 0/37, observed
agent/image startup mismatch, unknown completion after killing the host launcher
and independently removing the container, and concurrent Claude/Pi launches
sharing unrelated transcripts. Actual packaged launchers, generated profiles and
container CLIs supply these results, not fake runtime responses.

Linux deliberately checks system Podman locations before ambient PATH. The
fixture therefore selects its existing delegating gate through the public
`WRIX_CONTAINER_RUNTIME` override, rather than relying on PATH shadowing. The
gate still executes the real selected Podman binary. The regression proves that
no named container exists before release and that it appears afterward in both
launch modes. Both the original and native harnesses required this selection
correction; the original assertions and deadlines were unchanged. Darwin retains
its qualified PATH-based gate.

The Rust harness keeps each process-group leader unreaped until cleanup, uses
separate output files to avoid pipe deadlock, and preserves the original
120-second launch/probe and 30-second cleanup/runtime-command deadlines. Darwin
accepts zombie-group EPERM only after observing the exact held group entirely
zombie; other signaling or observation errors fail. Independent named-container
cleanup is attempted after process cleanup and after assertions fail.

Actual Linux owning acceptance used only the approved aarch64 builder: a stock
rootless-Podman NixOS guest under QEMU/TCG, with guest-only mappings. An
unchanged warm campaign ran native seven-of-seven and original four-of-four,
including every live scenario in both modes, in 442.22 seconds overall. The
original source, correction and hashes were archived before retirement. Cold
preparation timeouts, original failures, metadata-registration errors and the
oversized guest failure remain evidence, not passes. Real images were prepared
and their configuration digests verified separately; this is warm runtime
acceptance, not cold image-import performance or x86_64 installed-store proof.
