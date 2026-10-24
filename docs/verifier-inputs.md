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
scopes include newly added resources. Every Nix-backed family retains all
repository Nix definitions, `flake.lock`, `loom.toml`, patches and shared
dispatch/protocol scripts, so toolchain and image-definition edits still select
all genuine dependents. There is no target-name exclusion filter.

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
package uses pinned upstream source without local patches. `wx-dhmlk` tracks a
current upstream regression: check discovery repeats per annotation after group
queries, and system discovery is per annotation. Existing query-sharing
assertions remain intact and currently fail; they are not relaxed to land this
change. Tier identities remain separate. Unknown declarations remain always-run;
malformed declarations and failed discovery block execution. No persistent
discovery cache, tier change, marker exception or skip-policy change is
introduced.

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
