# Lightweight profile catalogue

`mkProfileImages` remains a realized manifest: its source/configuration output
contexts are intentional. It is not a cheap catalogue, and changing its
semantics would break consumers expecting ready-to-use paths.

`mkProfileCatalogue` is a separate, versioned producer:

```nix
let
  catalogue = wrix.mkProfileCatalogue {
    rust = {
      pi = rustPiSandbox.image;
      direct = rustDirectSandbox.image;
    };
    python.pi = pythonPiSandbox.image;
  };
in {
  packages.profile-catalogue = catalogue;
  packages.selected-rust-pi = catalogue.provisions.rust.pi;
}
```

Discovery is `wrix.capabilities.profileCatalogue == 1`. The JSON shape is:

```json
{
  "schema": 1,
  "profiles": {
    "rust": {
      "pi": {
        "ref": "wrix-rust-pi:example",
        "source_kind": "docker-archive",
        "realization": {
          "path": "/nix/store/...-profile-images.json",
          "derivation": "/nix/store/...-profile-images.json.drv"
        }
      }
    }
  }
}
```

The realization is one immutable **single-entry manifest**, not an image path
that might be unresolved. Its recipe retains the matching source and generated
ProfileConfig dependencies. The catalogue retains only that recipe; the
materialized manifest retains actual output contexts. A root on the latter
therefore retains the selected image/configuration closure. No cache assumption
or arbitrary installed executable resolves the paths.

`catalogue.catalogue` exposes the structured recipe-only data at evaluation
time; `catalogue.provisions.<profile>.<agent>` exposes the explicit materialized
Nix output. Merely serializing `catalogue.catalogue` remains lightweight.
Sources/configurations must be derivations, agent keys must match their images,
and metadata that would smuggle an output dependency is rejected. The original
manifest helper and launch formats are unchanged.

## Explicit selected provisioning

A trusted consumer selects an exact profile/agent, rejects unsupported catalogue
schemas, and verifies that `nix-store --query --outputs DERIVATION` returns the
bound manifest path as its sole output before effects. Then it can run:

```bash
nix-store --realise "$derivation" --add-root "$private_root" --indirect
```

This retains normal Nix signature/authentication and configured remote
realization. Retain separate roots for distinct selected output identities;
provisioning a newer consumer candidate is not authority to unroot a running
selection. After realization, load the single-entry manifest, check its
profile/agent/ref/source-kind against the selected catalogue entry, and parse
its immutable ProfileConfig through the normal launch boundary. Existing
runtime/controller/native readiness and image checks remain necessary.

Catalogue rendering/listing is not preparation or readiness. Neither catalogue
construction nor image provisioning authorizes controller/service upgrades or
host policy changes. Do not put catalogue JSON into the legacy
`LOOM_PROFILES_MANIFEST` variable: its parser expects materialized entries.

## Bounded proof

`tests/profiles/catalogue.nix` covers the full matrix with eight deliberately
unbuildable fixture image sources and one tiny selectable source. It proves
recipe-only contexts, exact entry selection, source/config binding, malformed
input rejection and per-entry invalidation. The same check renders a catalogue
from actual `mkSandbox` base/Rust/Python × Direct/Claude/Pi constructors,
without building their images.

On the consuming Mac, the real nine-entry catalogue built only one output in
**7.64 seconds**, with a **2,360-byte NAR**. That timing includes evaluation and
first rendering; it is not image provisioning or a complete shell measurement.
The catalogue references nine manifest derivations, not image outputs. Recipe
closure metadata still occupies space; the small JSON size is not the size of
its entire retained recipe closure.

`test-ci:test-profile-catalogue-realization` runs the real-Nix system fixture in
`tests/profiles/catalogue-realization.sh`. It checks the recipe/output binding,
copies the catalogue into an empty isolated store using a signed fixture cache,
verifies no entries are materialized, realizes only `rust/direct`, checks its
actual source/config validity and GC roots, collects garbage in that isolated
store, verifies the selected payloads survive, and confirms the other eight
entries remain absent. Signature checks stay enabled. The temporary key is
trusted only for this isolated store; no host cache or persistent trust
configuration is changed. It does not launch a container or claim native runtime
acceptance.

## Loom migration still required

Loom's checkout was inspected, not modified. Its current `nix/lib.nix`
`mkProfileManifest` reconstructs context-bearing source/config fields for all
nine images. Its `profile_manifest/provision.rs` bootstrap can additionally
build the published operator's `profile-images^*` factory. Merely exposing this
Wrix producer does not remove either fan-out.

The upstream consumer work is tracked separately as `wx-j8xzf`:

1. Export a catalogue from the consumer-pinned Wrix API, rather than retaining
   all image outputs in the devshell/package wrapper. Keep the old manifest
   format for callers explicitly requesting the full materialized matrix.
2. Parse catalogue metadata without constructing a ready `ImageEntry` containing
   unresolved paths. Select a profile/agent before provisioning; use a distinct
   realized selection type only after output provenance, realization and roots
   succeed. Passive listing/ordinary CLI commands do not provision the matrix.
3. Keep the current published-operator association, clean-source, commit
   signature, bounded retry and source-recheck guards around any automatic
   factory operation. Replace the aggregate `profile-images^*` fallback with a
   selected-entry operation, not a fallback to another image or client.
4. Bind Loom's native-preflight availability to the exact realized immutable
   ProfileConfig. A catalogue annotation is not evidence that worker probes
   passed; mandatory Rust/Pi/native/security verification still runs and fails
   closed. Preserve this through driver subprocesses and legacy migration.
5. Retain selected closures across concurrent jobs, source updates and Nix GC;
   prove real remote realization and distinct source/config invalidation in a
   locked consumer fixture. Explicitly requested full-matrix provisioning
   remains separate maintenance, not shell entry.

The host Container/Swift boundary, narrow Rust binary packaging, and this image
catalogue boundary are three separate dependency/cost issues.
