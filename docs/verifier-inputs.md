# Verifier input discovery and measurements

## Definition-derived scope

`tests/lib/inputs.nix` binds resource operands to their input projections.
Source-only checks in `tests/verify/profiles-eval.nix` read the same resource
objects they describe. Builder CI definitions bind library and fixture directory
operands to their imports and scripts. `tests/lib/image-resources.nix` separates
browser configuration, assembled image, and Rust CLI/service resource families.
Imports and fixture paths use the same directory objects as their projections.
Image-only and browser checks do not inherit the whole Rust workspace or every
spec; checks using the real host CLI or service image retain those dependencies.
Darwin browser guest launch checks retain Rust and image resources because they
invoke the real host CLI; Linux host-browser checks do not. Loom/profile image
wiring uses image resources rather than unrelated Rust sources. Legacy profile
verifiers still use conservative `browser-resources.nix` scopes. Directory
scopes include newly added resources. Image/browser families retain their owning
resource directories plus root flake/module and verifier-dispatch definitions,
`flake.lock`, `loom.toml`, patches and shared protocol scripts. They no longer
claim every repository Nix file as an input: service-only Nix edits select the
service-inclusive runtime matrices, not unrelated worker-image/browser checks.
Image resources explicitly include Beads wrappers and the native compiler
sources packaged by profiles. Other Nix-backed families retain their
conservative repository-wide Nix scope. There is no target-name exclusion
filter.

The existing target registries generate `--print-inputs` batch maps; there is no
separate target/dependency registry. Opaque definitions are omitted, not encoded
as empty known declarations. Verifier definitions separate script thunks from
metadata. The discovery app contains no target scripts or runtime references;
admitted execution realizes only the selected script through
`legacyPackages.<system>.verifyScripts`. Description construction does not force
opaque predicates/packages or browser, builder and image/tool evaluation. The
Nix-native `verifier-inputs` check exercises these boundaries, malformed
declarations, and the connection between resource operands, reads, and
projections.

Both check and system runners expose batched input queries. The consumed Loom
package backports the official tier-separated discovery fix and its regressions
through `patches/loom-runner-input-batching.patch`, retaining the current
upstream pin and supported runner protocol. `checks.loom-runner-inputs` executes
the patched upstream input-resolution tests. The unchanged packaged CLI
conformance requires one discovery query per runner group within each tier,
shared with the integrity audit, without changing System execution semantics.
Tier identities remain separate. Unknown declarations remain always-run;
malformed declarations and failed discovery block execution. No persistent
discovery cache, tier change, marker exception or skip-policy change is
introduced. See
[Loom troubleshooting](loom-troubleshooting.md#runner-input-discovery) for the
integration boundary of a full upstream update.

`verify:cli.shared-verifier-app` runs 44 Rust dispatcher and input-discovery
cases against the actual verifier, CI dispatcher, consumed Loom and Nix. Four
independent fixture repositories run concurrently. Each fixture has private
mutable state and copies of consistent read-only backups of Nix fetcher
metadata. Only a separate suite-owned Git object cache is shared; it contains no
links or alternates into caller files. Caller configuration and Wrix
admission/verdict caches are not inherited. Nix still resolves and verifies the
original pinned sources. Capability/skip authorization, invocation order,
foreign-platform inventory, malformed discovery and real builder-suite
conformance remain asserted. These external cases are ignored in ordinary Cargo
runs and explicitly selected by their owning verifier.

`verify:cli.browser-gate-scope` exercises the real consumed Loom planner and
production Wrix runners in disposable Git repositories. It checks changed
fixture/contract/lock/image/dispatch subjects in both lanes, opaque selection,
query batching, explicit/full execution and failed discovery. Unique failing
external browser/MCP package derivations provide cold realization evidence from
native Nix build logs, rather than successful fake gate output or warm-cache
absence. Inventory, descriptions and unrelated overview documentation leave
those boundary derivations unrealized. On Linux, cache source and owning-spec
changes also leave browser and profile-image boundaries cold in both lanes. This
is a system-boundary scoping test, not evidence of successful live browser
execution.

## Architecture selection

Architecture-neutral OCI, private-Nix and known-host checks use the executing
host's `checks.<system>` through `verify:sandbox.*`. Private-Nix artifact
inspection uses `test-ci:test-private-nix-artifacts`, whose images retain the
constructor's matching Linux guest architecture. On Apple Silicon these use
Darwin tools and the configured `aarch64-linux` builder, not an unrelated
`x86_64-linux` host. Explicit x86_64 native runtime qualification remains a
separate platform-specific requirement. `checks.native-verifier-systems`
executes the generated dispatch for both Linux architectures and Darwin.

## Admission and archive inspection

`test-ci` preflights declared platform/capability requirements before
constructing its build batch or resolving a single runner. Denied targets emit
classified skips without building. Admitted runner build failures still fail;
unexpected script exit 77 is not converted into an allowed capability skip.
Partial builds, duplicate invocations and verdict order retain their existing
behavior.

Docker archive inspection now stores only config, layer headers, digests and
source offsets. Queries decompress/seek the original archive as needed for a
requested file; no complete layer or decompressed archive is saved. Config and
manifest metadata are limited to 16 MiB each. Executable native producers need
one retained snapshot, not a snapshot plus unpacked layers. OCI inputs still
verify emitted blob digests/sizes and link to immutable payloads. This bounds
additional inspection disk by header/config size, not layer payload size; it
does not eliminate the original Darwin image archives or promise zero image
builds. Real CLI/service image checks and opaque always-run targets remain.

## Targeted notification iteration

The attention owner runs the complete `wrix-notify` Rust suite, the CLI and
sandbox libraries, the `native_notify` integration target, the packaged desktop
fixture, and the existing Linux-only native attention boundary. Unrelated
package integration suites remain in the unchanged canonical workspace Nextest
baseline; they are not rebuilt merely to iterate on notification acceptance.
Development uses the owning checks; complete workspace verification remains a
landing obligation.

Standalone Rust tools retain canonical workspace manifests and lock bytes plus
the complete selected crate and declared external resources. Unused members have
empty library entry points for Cargo metadata, not copied implementation files.
Local Cargo dependencies are rejected rather than silently compiled from those
scaffolds. `checks.<system>.rust-tool-source` executes Cargo metadata, compares
all selected files and canonical manifests, rejects leaked unused sources, and
exercises the local-dependency refusal. Changing this projection changes tool
producer identities; it is not a claim of unchanged image derivations or zero
cold landing cost.

## Rust check storage

Rust lint and test derivations, including the consumed Loom contract checks,
retain success outputs rather than exporting unused Cargo target caches. The
separate `cargoArtifacts` dependency cache is still built and reused. The
profile-config parser check uses the inherited Cargo profile, avoiding a second
debug build beside release dependencies. Neither change disables checks or
changes their selected cases.

Nextest schedules the existing long SSH backpressure regression early rather
than shortening its waiting budget. A matched local all-target experiment kept
eight threads, all groups and all test bodies unchanged: both runs passed 1,594
tests with the same 150 skips and identical passed IDs. Execution decreased from
165.501 to 141.814 seconds; the regression still ran for 31.27 seconds, moving
from the final result to result 330. These are warm native execution
measurements, not cold Nix setup or whole-push timings.

Maintenance fixture requests and replies use bounded newline-delimited JSON, not
peer EOF as message completion. Native controls keep the peer open while
checking complete arguments, Unicode/newlines, streams, failure status and
payload bounds. The existing client/server deadlines are unchanged.

The native backend-test package selects its immutable output target directory in
`prePatch`, before Crane restores dependency artifacts in `postPatch`. Changing
it later in `buildPhase` strands the restored target and starts a second
dependency build. The `cargo-target-handoff` Nix check executes the package's
actual handoff and Crane's restoration hook, including a late-handoff negative
control. Its compile-only source lane omits unused Nextest runner configuration,
so scheduling edits do not recompile these seven binaries. Ordinary Clippy and
Nextest checks retain that configuration. Native Nix mutation controls preserve
Rust/test/lock invalidation and complete runtime fixture staging. Compiled test
sources and embedded executable paths remain in the output; unused target
artifacts are still removed after packaging.

Lock-file and extra-source inputs are copied to content-scoped paths before
staging. This prevents string-valued paths inside a store-backed source tree
from pulling the entire parent snapshot into a build identity; Nix literal-path
inputs already have this property. Store-backed regressions cover both forms:
test/example changes keep binary and dependency-cache identities stable,
unrelated documentation changes also preserve check identities, production
changes invalidate binaries but not dependencies, and changed lock bytes still
invalidate dependencies. Directory overlays merge at their declared paths rather
than nesting another source directory.

For the artifact-export policy alone, a controlled real Cargo fixture kept all
compiler, source and test inputs fixed. Its Clippy output decreased from 635,112
NAR bytes to 96 and its Nextest output from 605,288 to 96. The latter's
reference closure decreased from 1,579,712,128 bytes to 96 because its unused
artifacts retained compiler paths. Binary and dependency-cache derivation
identities were unchanged. These are fixture output/reference sizes, not
whole-push peak-disk or timing claims.

Wrix's production source projection excludes crate test/example directories and
root test fixtures. Lint/test sources restore the examples and complete root
`tests` directory, including Nextest setup scripts and their resources; explicit
fixture executables share a separate example source and one Cargo build for the
controller and runtime adapters. Nix source regressions verify that changing
tests/examples leaves production source identity unchanged, changing production
code does not, and a leaking projection is rejected. Production bootstrap script
inclusion and complete example/test staging remain checked against the original
sources. Packaged Nextest initializes an isolated Git worktree in its disposable
build source so in-process repository-policy checks have the same Git context as
ordinary Cargo runs, without personal Git metadata or changes to production
packages.

## Metadata and artifact lanes

Source-kind and ownership-label constructor matrices run without realizing image
payloads. Hook closure checks inspect `materializedRoots`, not ten complete
profile/agent archives. `checks.<system>.image-verifier-realization` rejects
image-source string contexts in these scripts, and exercises its detector with a
real image source. The metadata assertions include negative source/label
fixtures. These are deliberately not claims about emitted bytes.

The original emitted-descriptor/digest, label and hook-layer matrices remain
available as System targets: `test-wrix-images-source-artifacts`,
`test-wrix-image-label-artifacts`, and `test-prek-hooks-artifacts`. They are
registered through the normal `test-ci` dispatcher and annotated in the image
spec. Layer membership, generated-state, archive codec and native runtime
coverage are not removed. Production image formats are unchanged.

On the inspected Darwin host, the three affected targets' combined unique
runtime NAR closure decreased from **14.72 GiB to 5.44 GiB** (about 63%). The
new hook check still needs real profile package closures; this is not a claim of
zero toolchain builds. The original broader 39-runner batch retained **30.5
GiB** across 713 paths. These figures exclude build-only inputs and temporary
space; they are not whole-store allocations or promised GC savings, since other
roots may retain the same paths.

The dispatcher also removes its temporary runner GC roots on handled INT, TERM
and HUP, not only normal exit. Its interruption regression starts a real batch,
confirms the runner symlinks exist, terminates its owned process group and
requires the directory to disappear. SIGKILL cannot run cleanup; abandoned roots
from killed older dispatchers still need operator review. No lifecycle,
service-update or installed-software roots are removed by these changes.

## Bounded-inspection comparison

For the cache-recovery paths in `61589a5f..92c85cb4`, intersecting production
`test-ci --print-inputs` maps with the changed files reduced affected known CI
apps from **43 to 4** (of 46 known declarations). The remaining apps are the two
real-CLI install checks and the service-inclusive source-kind/label matrices.
This is a declaration-intersection measurement, not a completed whole pre-push
run; opaque targets are unchanged and are not counted in these totals. Changes
to Nix definitions still intersect every Nix-backed declaration.

A real gzip Docker fixture containing a 4,198,400-byte emitted layer, config and
manifest was inspected with the helper at `92c85cb4` and with this change:

| Inspection output                               |    Before |  After |
| ----------------------------------------------- | --------: | -----: |
| Regular-file logical bytes                      | 4,198,588 |    400 |
| Allocated file bytes (Linux fixture filesystem) | 4,210,688 | 20,480 |

The original compressed source is excluded from both output totals. The
regression uses a 4 MiB file and requires inspection output below 8 KiB of
logical data; it also verifies requested payloads against emitted bytes.
Headers, paths and filesystem block sizes affect exact totals. This does not
measure Darwin host free space, prove a 300 GB saving, or claim a universal
runtime speedup: streaming scans and compressed seeks trade CPU for bounded
additional disk. Run `verify:images.inspection-artifacts` for current coverage.

## Historical check-only worker comparison

The measurements below cover the earlier six known declarations and check-only
opt-in. They are not measurements of the current browser/system integration.

Base: published `a43ff7d08b40c38bfaeaae73d1b577e3899e18de`, consuming official
Loom `7d4fcbd`. The comparison uses seven annotated targets, the real Wrix
runners/assertions, and committed synthetic subject changes through
`loom gate verify --diff`. Five warmed, interleaved samples per variant were
measured without tracing; separate traces counted Nix frontend requests. This is
a representative scoping experiment, **not a whole-repository or live-host
pre-push speedup claim**.

| Changed subject                 | Targets before → after | Nix requests before → after | Median seconds before → after |
| ------------------------------- | ---------------------: | --------------------------: | ----------------------------: |
| `README.md`                     |                  7 → 1 |                       6 → 3 |                14.007 → 0.769 |
| `flake.lock`                    |                  7 → 7 |                       6 → 8 |               25.306 → 27.784 |
| `tests/builder/key-material.sh` |                  7 → 4 |                       6 → 5 |               12.357 → 16.747 |

Every selected target passed. The opaque notification check remained selected in
every case. All three builder checks remained selected for the builder fixture
change; all seven remained selected for the lock change. The builder
executables' three derivation identities are unchanged from the base.

The baseline has two outer Nix runs, three source-check evaluations, and one
shared CI build request. Discovery adds exactly two outer Nix runs, one per
check runner group, shared with the integrity audit. An independent
documentation change removes all three evaluations and the CI build/execution. A
builder fixture change removes the three unrelated source evaluations, but
retains CI execution. A genuinely shared lock change removes no work and pays
the two queries; this is an explicit cost, not a universal speedup.

Image/archive execution was noisy: baseline lock samples ranged from 6.594 to
50.282 seconds, and builder-fixture samples from 7.813 to 63.998 seconds. Do not
interpret their median differences as isolated query or execution costs.
Separate traced discovery totaled 1.19–1.33 seconds across both queries, but
traced durations are not the untraced headline timings. Warmups reported no new
derivation builds; the shared build request checked already-realized closures.
These samples provisioned no live containers and establish no new live-host
acceptance.

## Reproduction and evidence

Use the refreshed, declared Wrix devshell/runtime for these commands and normal
pre-push verification. Updating `flake.lock` does not replace the Loom binary in
an already-running worker or shell. A stale ambient Loom was observed issuing
per-criterion input queries; the controlled comparisons use the consumed
`/nix/store/104wy3ijbw7p3nipdxzi0jig2bjibyw7-loom-0.1.0/bin/loom`, not that
older runtime.

```bash
nix build .#checks.x86_64-linux.verifier-inputs
nix run .#verify -- cli.browser-gate-scope
nix run .#verify -- cli.shared-verifier-app prek.ci-platform-policy prek.ci-batching
nix run .#verify -- --print-inputs devshell.no-prek-install notifications.focus-target-envelope
nix run .#test-ci -- --print-inputs test-linux-builder-sshd-hardening test-security-execution-metadata-lifecycle
```

The shared verifier self-tests cover real changed-source failure, owning-spec
inclusion, new Nix definitions, unknown always-run, malformed protocol failure,
lookup/normalization, description-only execution, fresh discovery in separate
invocations, and system sharing with one receipt per criterion. Existing result,
sandbox capability, CI batching, and push-marker conformance remain in place.

The original session used scratch measurement logs and per-target receipts.
Scratch is ephemeral, so those paths are not durable evidence or reproduction
inputs. The table above records that historical experiment, not a current
verification result. Run the verifier commands for current evidence.

An existing upstream defect, `wx-1x5ns`, drops runner-owned logical targets on
explicit `--files` scopes despite discovery. These regressions exercise the
actual pre-push `--diff` path; no consumer bypass or upstream fix is included.
