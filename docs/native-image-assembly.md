# Native OCI assembly

`wrix-image-tools oci-layout` consumes the existing dockerTools configuration
and composes immutable parent/package blobs. It rejects whole-image archive
parents. Final composition reads parent manifest/config metadata, links cached
blobs, and stats the customization tar without opening its payload. Package
archives are hashed while written; no second hash or copy pass is required.

The Rust package writer emits POSIX/PAX headers, including long and Unicode
names, binary-name surrogateescape handling, fixed ownership/timestamps, sorted
path traversal, executable modes, and materialized hardlinks. It never descends
through source symlinks. Its synchronous writer has no producer thread or pipe
whose buffered lock can deadlock against a consumer. Early and late I/O failures
propagate, and failed CLI production leaves no published output.

The native tests retain all four previous OCI-builder scenarios: reproducible
honest package digests and headers; parent/package reuse and Env overlay;
archive-parent refusal; and generated changes retaining shared package identity.
Additional controls cover unreadable cached payloads, strict digest/argument
parsing, timestamps, exact PAX headers, binary names and failed production.
`tests/sandbox/oci-layout.nix` exercises the actual packaged CLI and production
Nix factory with real dockerTools metadata. It verifies every emitted config and
layer digest/size, diffIDs, hardlink payloads, changed customization blobs, and
actual shared package-file identity across two generated variants.

The frozen `oci-pax-header.bin` fixture comes from upstream Python 3.14 tarfile
PAX serialization, not the new serializer. Its input is a regular entry named
`/nix/store/` plus 130 `a` characters plus `/café`, mode `0751`, UID
`123456789`, GID `20`, size `5`, mtime `-1`, uname `buildér` repeated six times,
and gname `staff`. Its SHA-256 is
`9b921e94b1a28db7f1fdf17622bdd8b6c9cf6f3aa89853995badba4fab2a7505`. An
independent original/native package comparison also produced byte-identical
layers with long names, Unicode names, symlinks and nested prefix-order cases.
The original four-case Python suite passed before removal.

`docker-stream` exports these cached blobs synchronously as Docker archives. The
Nix adapter preserves the existing raw/compressed stream interface, tag
overrides, inherited Env overlay and Docker history comments. Custom-pipeline
children reference parent OCI layouts, not parent archives: the packaged test
successfully emits a child whose declared archive parent is an always-failing
fixture, including with the default layer planner. The default planner retains
its inherited-layer budget using the genuine parent OCI index and manifest,
without realizing the parent archive. Manifest bytes are checked against the
index's SHA-256 descriptor; malformed digests, altered bytes and exhausted
budgets fail closed. The upstream archive-counting fallback remains available
for non-native dockerTools callers.

Native failure controls cover early/late producers, consumer write/finalization
errors, and a real executable blocked on a kernel pipe before its reader closes.
The packaged archive owner verifies raw/compressed equality, real Skopeo
conversion and digest integrity on Darwin and Linux, plus an actual failing
producer pipeline and a deliberately broken no-pipefail negative control.
Upstream six-case Python pipe regressions passed before their replacement.

This does not establish full cold OCI reuse, physical allocation savings, or
production/platform acceptance. Those retain their independent emitted-artifact
and runtime owners.
