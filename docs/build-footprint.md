# Verification build footprint

Push verification retains its existing assertions, signatures, source
projections, platform requirements and deadlines. Reductions must distinguish
runtime closure, build inputs, temporary allocation and native execution time.

## Normal developer suite

Run `bin/test-native` for the complete workspace and all native targets. It
includes Cargo compilation and Nextest's automatic fixture setup in its measured
wall time, fails the five-minute acceptance budget even if tests pass, and has a
ten-minute hard stop including cancellation. It accepts no test filters. Setup
and test failures retain their nonzero exit status.

Packaging, image and platform acceptance remain required separately at the end
of a batch. Their costs are not native-suite timing, and an unfinished
publication gate is not a successful test result. Verification commands must be
diagnosed at the ten-minute boundary rather than retried with longer timeouts.

## Internal Git runtime

Wrix's packaged command, controller tools and cache publisher use nixpkgs' Git
Minimal for their core Git operations. The Linux prek runner also uses Git
Minimal. The Darwin prek runner uses full packaged Git: real HTTPS HEAD queries
for the pinned Celler source stalled with the minimal transport, while the full
transport completed. Hook entries, signatures and certificate verification
remain unchanged. Workspace profiles and explicitly selected Python packages are
unchanged. Compiler/check dependencies are not removed or hidden.

The packaged-runner conformance check performs real SSH-signed commits and tags,
verification, worktree access, local transport push and clone. Existing CLI and
hook acceptance remains required; changing the dependency is not a substitute
for running those suites.

An aarch64-darwin comparison of actual built Wrix closures, based on the native
migration checkpoint before the subsequently published client/server and
recipe-only tooling boundaries, measured:

| Closure             |     NAR bytes |
| ------------------- | ------------: |
| Previous runtime    | 2,337,111,768 |
| Git Minimal runtime |   903,297,416 |
| Reduction           | 1,433,814,352 |

The previous Git dependency pulled Python, which retained Apple's SDK and LLVM
libraries. Removing that runtime edge eliminates about 61% of this closure.
These are serialized closure sizes, not unique physical disk savings or a claim
that compiler-dependent verification no longer needs an SDK. The final combined
client/server/catalogue candidate needs a fresh closure measurement; the table
must not be presented as that newer candidate's complete push footprint.

## Build cache and allocation

The CI app driver plans admitted targets once through Nix's locked-flake
evaluation cache and retains existing derivation metadata separately from
realized outputs. Rooting metadata imports only the planned store derivations,
not the whole flake through an impure `builtins.getFlake` call. It builds and
executes at most two selected app packages at a time, then releases that
window's temporary output roots. Ordered per-target verdicts, duplicate
invocations, capability admission, individual build failures and interruption
cleanup retain their existing behavior. This bounds simultaneously retained
build inputs; it does not omit image checks or claim that the complete cold
image matrix finishes in five minutes.

A successful dependency build is not necessarily a retained dependency cache:
normal result roots do not automatically retain build-time-only Cargo artifacts.
Keep the selected verification's actual dependency artifact rooted while the
remaining phases use it; do not add compiler artifacts to production runtime
closures or disable normal session-root cleanup.

The observed shared Cargo artifact was approximately 255 MiB compressed. That
size does not establish the peak allocation needed to restore, compile and link
it. Similarly, cached gzip-member reuse avoids inherited recompression but does
not eliminate the bytes in final archive outputs. Cold/warm and physical peak
measurements remain separate acceptance evidence.
