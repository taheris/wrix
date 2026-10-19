# Verifier input discovery and measurements

## Definition-derived scope

`tests/lib/inputs.nix` binds resource operands to their input projections.
Source-only checks in `tests/verify/profiles-eval.nix` read the same resource
objects they describe. Builder CI definitions bind library and fixture directory
operands to their imports and scripts. Browser/MCP definitions use
`tests/lib/browser-resources.nix` for their library, fixture and owning-contract
resources, including transitive Rust, image and packaging dependencies. Image,
profile and Loom-package constructors also project these resources, covering
indirect MCP image references without a browser-target-name filter. Directory
scopes include newly added resources. These projections deliberately include
repository Nix definitions, `flake.lock`, `loom.toml` and shared
dispatch/protocol scripts; browser coverage is conservative across all specs,
tests and library resources, not optimized to exclude unrelated changes within
those directories.

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

Both check and system runners opt into batched input queries. The consumed Loom
package uses the pinned upstream source without local patches; query-cache
priming and system-group batching depend on that upstream version. Tier
identities remain separate. Unknown declarations remain always-run; malformed
declarations and failed discovery block execution. No persistent discovery
cache, tier change, marker exception or skip-policy change is introduced.

`verify:cli.browser-gate-scope` exercises the real consumed Loom planner and
production Wrix runners in disposable Git repositories. It checks changed
fixture/contract/lock/image/dispatch subjects in both lanes, opaque selection,
query batching, explicit/full execution and failed discovery. Unique failing
external browser/MCP package derivations provide cold realization evidence from
native Nix build logs, rather than successful fake gate output or warm-cache
absence. Inventory, descriptions and unrelated overview documentation leave
those boundary derivations unrealized. This is a system-boundary scoping test,
not evidence of successful live browser execution.

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
