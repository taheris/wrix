# Verification build footprint

Push verification retains its existing assertions, signatures, source
projections, platform requirements and deadlines. Reductions must distinguish
runtime closure, build inputs, temporary allocation and native execution time.

## Internal Git runtime

Wrix's packaged command, controller tools, cache publisher and prek runner use
nixpkgs' Git Minimal for their core Git operations. Workspace profiles and
explicitly selected Python packages are unchanged. Compiler/check dependencies
are not removed or hidden.

The packaged-runner conformance check performs real SSH-signed commits and tags,
verification, worktree access, local transport push and clone. Existing CLI and
hook acceptance remains required; changing the dependency is not a substitute
for running those suites.

An aarch64-darwin comparison of actual built Wrix closures measured:

| Closure             |     NAR bytes |
| ------------------- | ------------: |
| Previous runtime    | 2,337,111,768 |
| Git Minimal runtime |   903,297,416 |
| Reduction           | 1,433,814,352 |

The previous Git dependency pulled Python, which retained Apple's SDK and LLVM
libraries. Removing that runtime edge eliminates about 61% of this closure.
These are serialized closure sizes, not unique physical disk savings or a claim
that compiler-dependent verification no longer needs an SDK.

## Build cache and allocation

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
