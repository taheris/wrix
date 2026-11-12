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

## Production source isolation

Client, service and controller binaries consume the normal/build
local-dependency closure of their selected packages, including optional and
target-specific edges. Unselected implementations become empty Cargo target
entrypoints; the original manifests, lock and complete target inventory remain
intact. Controller runtime tools no longer pull the standalone image/verifier
tools into their implementation package. The full workspace package remains
available unchanged.

Clippy and Nextest retain the full sources and fixture overlays. Source-owning
checks compare complete production derivation keys across unused implementation
and fixture edits, check that selected/dependency edits invalidate those keys,
execute aliased workspace/build dependencies, and compare the real workspace's
Cargo target inventory for every production projection. This prevents verifier
implementation edits from rebuilding clients, service images and their dependent
image tiers.

Automatic native-fixture compilation uses the same implementation projection.
The original production-package fixture-exclusion assertions run in the required
`native-fixture-production-isolation` Nix check, covering the foundation,
client, service and controller packages. They no longer force a separate
full-workspace production build merely to start the native suite; packaged
example unit tests remain enabled during fixture construction.

## Archive work

The default cached Docker gzip members and archive tail use deterministic
`pigz -1 -n`. Explicit caller-selected compressors remain unchanged. A member is
reused only when its timestamp and complete compressor recipe match; the owner
check supplies invalid legacy-recipe members and verifies that they are rebuilt.
Decompressed archives remain byte-identical to the native stream.

A bounded two-core comparison on the same 5,566,197,760-byte emitted Rust store
tar measured default compression at 59.10s / 1,523,160,354 bytes and level one
at 19.36s / 1,692,817,188 bytes: 3.05 times faster compression, with 11.1%
larger gzip output. These are compressor-stage measurements, not whole-push or
physical allocation savings.

Prepared artifact indexes retain bounded gzip-member restart points. Raw tar
queries seek directly; gzip queries restart at the nearest validated member
instead of decompressing every parent tier. Preparation still validates the
entire transport, including all gzip trailers and tar headers. Restart metadata
is bound either to a matching full-source SHA-256 or to an immutable store file:
the source and each store-entry component are root-owned and read-only, under a
root-owned store with protected directory entries. Mutable source changes fall
back to sequential validation, and legacy indexes remain readable. No inode or
host timestamps enter the persisted index. On the same emitted Rust archive,
extracting the retained store-roots file measured 12.24s before and 1.06s with
restart indexing, with byte-identical output. Preparation remained approximately
40s; this is a query improvement, not a claim that preparation became free.

## Nix output staging

OCI Nix builders stage inside their reserved output root and rename the
completed contents on the same filesystem. Nix protects that active build output
from GC; the builder removes its owned output on failure. This avoids the
temporary copy and cross-volume move of multi-gigabyte store tars through
`NIX_BUILD_TOP`. Standalone unreserved destinations retain the separate
private-build-directory wrapper and its original/fixed private-GC controls. The
generic Rust atomic writer is unchanged.

Both-platform Nix owners exercise the reserved-output entrypoint,
same-filesystem staging, existing/dangling destination refusal, early/late
failure cleanup, and unchanged emitted layouts. No unregistered top-level store
staging directory or additional GC root is created.

## Build cache and allocation

The CI app driver plans admitted targets once through Nix's locked-flake
evaluation cache and retains existing derivation metadata separately from
realized outputs. Rooting metadata imports only the planned store derivations,
not the whole flake through an impure `builtins.getFlake` call. It builds and
executes one selected app package at a time, then releases that window's
temporary output root. A completed check no longer holds image inputs while an
unrelated large producer in the same build invocation is still running. Ordered
per-target verdicts, duplicate invocations, capability admission, individual
build failures and interruption cleanup retain their existing behavior. This
bounds simultaneously retained build inputs; it does not omit image checks or
claim that the complete cold image matrix finishes in five minutes.

Hermetic artifact checks execute their unchanged assertions in source-bound Nix
check derivations. Their CI launchers retain only the genuine successful check
output, not the image runner's multi-gigabyte closure. Runtime, host-state and
store-query checks keep their live runners; capability admission remains live
and precedes any build. Changes to an artifact, checker or input produce a new
check derivation, and failed checks cannot produce a passing launcher.

During a batch, completed check-result roots survive window rotation without
retaining old image inputs. Normal exit and interruption remove those temporary
roots. Constructor conformance checks exercise actual success/failure, input
invalidation and closure separation; the native batching owner verifies
retention and cleanup. This is ordinary Nix check caching, not an admission
receipt.

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
