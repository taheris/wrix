# Client/server compilation boundary

`wrix-service` retains shared protocol, authority/registration locks, policy and
lifecycle primitives used by both the CLI and trusted roles. The new
`wrix-service-server` owns Rama gateway traffic, management serving, fixed-role
forwarding, controller issuer/serving state, compiler startup/forwarding and all
five private process entrypoints. Its Rama dependencies are unconditional. There
is no alternate security feature set or arbitrary installed-tool fallback.

The six client executables and their normal/build dependency graph do not reach
that server crate or Rama. Selecting six binaries alone did not achieve this:
previously `wrix-cli` linked the shared service library that compiled the
engines. Wire formats, authentication, authority ownership, readiness,
revocation and lifecycle semantics are unchanged. Shared controller authority
and lifecycle primitives deliberately remain common, rather than duplicating
their checks.

## Source and verification ownership

The server crate explicitly registers the existing five binaries, two examples
and 17 integration targets. Module `#[path]` declarations and target paths
retain the implementation/verifier locations under `crates/wrix-service`; Cargo
compilation ownership is now separate. Automatic target discovery is disabled in
both packages to prevent duplicate or accidental target registration, not to
skip tests. The metadata check compares every existing integration source name
with the registered server targets.

`lib/services/rust-client-source-filter.nix` removes only server implementation
subtrees from the selected production client source. Cargo manifests remain
available for workspace resolution. The client dependency cache builds selected
normal targets; it does not serve as verification evidence. Full workspace
checks retain their independent original all-target dependency checks, test
compilation, complete staged source and Nextest execution. Runtime packages and
native fixture examples select the new server package explicitly.

Evidence:

- `client-server-dependency-boundary` reads real Cargo metadata from the
  projected production source, checks the normal/build closure and unconditional
  server dependencies, and rejects an injected forbidden dependency.
- `client-package-boundary` builds/executes the actual six-binary output.
- `rust-source-fixtures` checks server-only exclusion, client edit invalidation,
  independent dependency artifacts and unchanged full verification construction.
- The pre/post Nextest inventories contain exactly **1,728 tests**, with
  identical ignored flags after accounting for package ownership and the
  compiler startup module prefix. No tests were removed or duplicated.
- The full default native run passed **1,578 tests in 287.576 s**; the original
  **150 ignored/platform cases** remain unchanged. This is not evidence of
  running the separately gated VM/system acceptance campaigns.
- Workspace all-target checking and strict Clippy passed, including the server,
  examples and all integration targets, on native Darwin and through Nix on the
  existing aarch64-linux builder. The three server route compile-fail doctests
  passed. Native `nix flake check`, packaged-client inventory, dependency graph
  and host-service binding checks passed. Nextest resource/setup filters cover
  both package owners after the move.

## Landing verifier

The repository source-isolation probe canonicalizes its copied fixture before
passing paths to Nix, including when temporary-directory ancestors are symlinks
on Darwin. Its regression test enters through a symlinked parent and still
checks preserved executable bits, links and the untouched original.

All four source variants are prepared before evaluation. A single Nix process
shares the unchanged nixpkgs import, while each variant retains its own complete
flake and every existing invalidation assertion. This avoids both repeated
imports and cached reads of a fixture mutated during evaluation. Bounded local
observations were 51.78 s for separate evaluations, 54.31 s for batching alone,
and 42.32 s with the shared import; these are not controlled benchmark results.
No landing checks or approval requirements are removed.

## Bounded measurements

One dependency-cold shell build, one warm client source edit and one server-only
edit used `tests/client-boundary` on the consuming aarch64-darwin Mac. The
common Nix/toolchain/vendor inputs were warm; the new client dependency artifact
was absent. This was not an empty store, locked real consumer, direnv activation
or runtime provisioning measurement. Temporary comment edits were reverted.

| Operation                                        | Wall time | Observed work                                                                                                    |
| ------------------------------------------------ | --------: | ---------------------------------------------------------------------------------------------------------------- |
| First selected-client shell realization          |   65.15 s | 25.94 s dependency compilation, 19.42 s client compilation, plus Nix/evaluation/staging                          |
| Client entrypoint source edit, dependencies warm |   44.08 s | 31.13 s client compilation; same dependency artifact reused                                                      |
| Gateway server-only source edit                  |   10.09 s | Six small binding/wrapper/shell outputs; compiled client derivation unchanged, server runtime derivation changed |

The client dependency artifact is **124,689,480 NAR bytes** and the six-binary
output **41,864,360 NAR bytes**. These are logical output sizes, not allocated
physical disk growth or whole-shell closure size. Binary size is essentially
unchanged from the prior six-binary package: the optimization removes
compilation work that the linker could already discard.

The historical 99.02 s full-workspace realization and rejected 108.08 s
selected/build-only trial are described in
[packaging costs](client-packaging-costs.md). The new cold observation is lower,
but those runs are not a controlled isolated speedup comparison. The 44.08 s
warm edit does **not** establish improvement over the earlier 43.73 s warm
realization. No repeated campaign was used to select a faster sample.

Server edits still change the pinned controller/runtime recipes, so lightweight
binding/wrapper outputs must update. They do not compile Rust clients or realize
those host/runtime recipes. The measurements did not start/upgrade shared host
services, change host policy, demand Container/Swift outputs, or provision an
image matrix. Loom's eager manifest fan-out and locked real-consumer evaluation
remain separate boundaries.
