# Client and host tooling boundary

The consumer pin still supplies `wrix`, `WRIX_BIN` and immutable project
configuration. It is not an instruction to install or upgrade a host controller.
The host controller binding now retains controller/runtime derivation recipes
and a small Nix/native-supervision management closure, not the tooling outputs.
Pool-tools metadata likewise retains the runtime recipe without demanding its
output. Recipe retention is intentional: removing all string contexts would make
cold or remote realization impossible.

`wrix service prepare` explicitly realizes those recipes and installs indirect
GC roots under native private state, without contacting supervision, running
Container, changing host policy or replacing a selected deployment. Each
recipe's sole output is checked against its bound path before either is
realized. Controller repair/update keep their explicit maintenance preparation
paths. Controller roots retain the management closure as well as the runtime.

Ordinary startup checks that selected tooling is already valid locally. A
missing candidate reports `wrix service prepare`, rather than provisioning it or
finding an arbitrary Container executable. An existing recorded deployment
selects its retained package instead of the caller's newer candidate. Native
ownership, versioned authenticated readiness, backend compatibility and
runtime/network policy checks are unchanged. Preparation is not readiness and
does not launch services. Older retained bindings remain readable.

## Bounded validation

The real downstream fixture is `tests/client-boundary/flake.nix`. Run from Wrix:

```bash
fixture=path:./tests/client-boundary
pin="git+file://$PWD"
drv=$(nix eval --override-input wrix "$pin" --raw \
  "$fixture#devShells.aarch64-darwin.project.drvPath")
nix derivation show --recursive "$drv" > graph.json
jq --arg root "$drv" -f tests/client-boundary/graph.jq graph.json > demanded.json
nix build --no-link --override-input wrix "$pin" \
  "$fixture#devShells.aarch64-darwin.default"
```

The graph audit follows demanded derivation outputs, not recipe-only retention,
and fails above 12,000 recipes. Its synthetic negative test and a client built
against a deliberately unbuildable Container are in
`tests/services/package-binding.nix`. Native supervision tests cover missing
prerequisites, partial metadata, provenance mismatch, explicit realization,
retention, foreign-unit refusal and stopped/selected identity preservation.

For independent real-Nix preparation, build the fixture's `preparation-binding`
output, then run `bash tests/client-boundary/prepare.sh RAW_WRIX BINDING`.
`RAW_WRIX` is the underlying compiled Wrix executable, not its production
wrapper (which correctly refuses ambient binding replacement). The test prepares
only tiny fail-if-executed fixture programs. It copies their recipes to an empty
isolated Nix store and their outputs to a signed fixture cache, exercises
passive CLI commands, then prepares by recipe and checks actual validity and GC
roots. Only that isolated store trusts the temporary signing key; signature
checks, sandboxing and host trust policy are not disabled. This tests nonlocal
output realization, not an SSH builder or a live Apple VM.

### Measurements on the consuming Mac

One shell build, not a repeated matrix campaign:

| Operation                                                              |                         Wall time |                                                                  Store growth |
| ---------------------------------------------------------------------- | --------------------------------: | ----------------------------------------------------------------------------: |
| First minimal fixture shell output                                     |                           99.02 s | 339,200,304 NAR bytes across 21 newly built outputs; volume delta 332,464 KiB |
| Explicit actual pinned controller/runtime preparation in isolated HOME |                            2.68 s |                              volume delta 324 KiB, three wrapper/join outputs |
| Tiny signed-cache / empty isolated-store preparation fixture           | about 1.2 s including cache setup |                                             approximately 2.8 MiB substituted |

The shell and client outputs were cold; common build dependencies were already
present. Container/Swift dependencies were also present when measuring explicit
host preparation. These are **not empty-store cold-build timings**. Volume
deltas can include concurrent filesystem activity; NAR bytes are logical output
size, not allocated disk. No shared-store eviction was used to manufacture a
cold run, and no full Swift compilation was launched for timing.

The actual project shell graph contained 8,991 recipes, of which 7,792 demanded
outputs; none demanded Container or any Swift-named build package. The graph
proof does not depend on cache warmth. An external copy of the fixture held Wrix
at one immutable store snapshot: changing `consumer.txt` changed only its source
artifact; changing project env changed its immutable configuration artifact.
Both preserved the exact client and host-binding derivation. Unrelated consumer
source is not an input to host-tool recipes. Building the project
configuration/image itself remains a separate cost, not included in the minimal
profile-only shell timing.

Targeted native tests passed, including authenticated controller repair and
fail-closed recovery. Workspace build/clippy and flake evaluation passed. One
full nextest attempt was bounded at 180 seconds after 413 passing tests with no
reported failure; it did not complete. Full native VM/security acceptance was
not rerun and is not claimed by these packaging measurements. No existing tests,
platform checks or security policies were removed or weakened.

## Separate Loom image fan-out

Loom's `nix/lib.nix` `mkProfileManifest` defaults to `base`/`rust`/`python` ×
`pi`/`claude`/`direct`. Its final JSON reconstructs context-bearing
`image.source` and `image.profileConfig` fields for every entry. Wrix's
`lib/sandbox/manifest.nix` `mkProfileImages` also retains both outputs;
`passthru.manifest` avoids building the JSON file at evaluation time, but
serializing that data with its contexts still demands the images. This is a
separate nine-image fan-out and remains unfixed here. Loom's checkout was only
inspected, not edited or activated.

The upstream API needs a lightweight **catalogue**, distinct from the current
realized manifest: versioned profile/agent metadata, source-kind and immutable
configuration/image recipes with output provenance, plus an explicit
selected-entry provision operation returning realized paths and GC retention.
The resolver must support nonlocal realization, invalidate consumer-dependent
images/configs when their inputs change, and admit compatibility/readiness
before launch effects. Loom must stop reconstructing context-bearing outputs for
every entry and explicitly provision only its selected profile/runtime (or an
operator-requested matrix). Simply stripping contexts from `mkProfileImages`
would leave unresolved paths and would not provide this API.
