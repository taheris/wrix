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

## Bounded store-key observations

A two-variant audit of source `6c09b60b` used the shipping image factory with
names `wrix-key-audit-a` and `wrix-key-audit-b`, tag `latest`, `maxLayers = 8`,
and distinct `wrix.audit.variant` labels. Both used the same
`writeTextDir "payload" "bounded canonical store key audit\n"` content. The
audit compared every store blob and `layers.json`, then inspected both real
Docker archives with Skopeo to verify their distinct labels.

The store-layer producers had different derivation identities but emitted the
same 10,240-byte tar blob, SHA-256
`fe021717a5242ce5c79501e17dd7db387013ba3c298bb9583d12915b098a809b`. This
identifies a bounded store-key reuse opportunity; it does not measure production
image or planner costs.

- Darwin's two blob paths shared one device/inode with link count three, so
  those paths did not allocate duplicate file contents.
- On the approved Linux builder, the two paths had different inodes on device
  65056, link count one each, and 24 allocated 512-byte blocks each. Read-only
  FIEMAP inspection reported separate, non-overlapping three-block extents with
  4,096-byte blocks. Thus this pair occupied 24 KiB of distinct guest-store data
  extents for identical contents. This is not an APFS host-allocation or
  whole-image savings measurement.

The original Darwin selected setup realized 17 producers in 16.96 seconds; its
exact warm invocation took 1.56 seconds. Linux's initial dry run selected a
22-producer graph, including tool setup, but its archive consumer failed because
Skopeo defaulted to an unavailable sandbox `/var/tmp`. Passing the existing
build's `TMPDIR` through Skopeo's `--tmpdir` option retained the real consumer
and passed in 3.74 seconds, followed by an exact warm invocation in 2.03
seconds. The successful Linux invocation reused the preceding setup: neither
timing is a cold whole-image baseline, and the failed setup's 27.12 seconds is
not a passing performance result.

The extent inspection used `stat -c '%d %i %h %s %b %B'`, `stat -f`, and
`filefrag -v` against the actual two immutable store blobs; it did not optimize
or modify the store. Broader planner-key canonicalization, the historical
original/native comparison, whole-push cold/warm costs, and host-unique physical
allocation remain unmeasured by this fixture. No production cache-key change
follows from this small audit alone.
