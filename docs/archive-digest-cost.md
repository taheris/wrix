# Archive digest setup cost

Darwin profile archives expose a `configDigest` co-output from the same archive
producer. Reading it does not perform a second whole-archive Docker-to-OCI copy.
The archive itself remains the shipping image source; emitted-archive, layer,
source-kind and runtime checks are unchanged.

The metadata helper uses the existing native archive-tail renderer, including
its inherited-history transformation. It creates a private OCI metadata view
with the actual layer blobs linked, not copied, and asks Skopeo to normalize the
configuration. This preserves Skopeo's timestamp and serialization semantics
without copying or recompressing layers. The digest and archive share the same
immutable stream inputs in one producing derivation.

Independent reconstruction from another layout is not sufficient. A real paired
probe found different leaf diffIDs between a cached archive and a re-realized
stream layout; its underlying historical cause was not established. Direct OCI
configuration hashing also missed archive history rewriting and UTC timestamp
normalization. Those failed comparisons were retained, not accepted as
equivalent. The co-output avoids inferring an existing archive's identity from a
later reconstruction.

Real archive tests compare the co-output with Skopeo conversion for compressed,
uncompressed, fixed-tier inherited and default-budget inherited archives. Parent
archive poisoning, compressor/mtime changes, payload equality and
producer-failure controls remain in the same owner.

Verifier scripts that consume Loom retain the pinned host CLI in their immutable
closures; listing targets and discovering inputs remain independent of that
runtime. A successful tiny Nix check output need not reference the program it
ran; preserving that marker alone allowed GC to remove Loom before a later
`build_flake_package loom` call. The consuming scripts now retain the actual
program, while that call still resolves and checks the current repository's
package. A Nix closure regression exercises this dependency.

This removes redundant metadata work; it is not evidence that an entire cold
push is below ten minutes. Whole-hook timing includes dependency acquisition,
archive emission, checks and verifier execution and is measured separately.
