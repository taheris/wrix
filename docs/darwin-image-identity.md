# Apple image identity

Apple `container` 1.5.0 exposes three distinct identities:

- `configuration.descriptor.digest` is the native OCI index identity; the
  top-level `id` is its hexadecimal suffix.
- `variants[].digest` identifies a platform manifest, not its configuration.
- The configuration digest hashes the exact configuration blob bytes.

The native-format fixture records all three from a read-only macOS 27.2
inspection/export. `skopeo inspect --config --raw` preserves the configuration
bytes; omitting `--raw` reformats JSON and changes its digest. Image lookup uses
stored references. A bare index ID or an invented `name@digest` reference is not
assumed to resolve through the installed CLI.

## Content observation

Explicit image installation observes the native index, exports that reference,
and verifies the saved archive's raw index digest before hashing its selected
configuration. Checking only the reference before and after export is
insufficient: an A → B → A change can otherwise attest B's configuration as A.
Subsequent configuration lookup is bound to the observed native ID: it resolves
an existing reference, then checks the ID in the same inspection used to select
the receipt. It does not assume the bare ID is a supported CLI selector.

Archive-bound receipts use version 2 under the host-private
`image/apple-config-v2/<guest-system>` directory. Passive inspection does not
create receipts. Earlier cache receipts remain untouched in their former
location and are not upgraded into stronger proof. Existing service owner and
selection schemas remain readable; this cache change does not invent image
provenance for historical owners or migrate their records.

## Before writer startup

Apple backend/gateway creation is separate from startup. The manager checks one
coherent stopped-container observation against the frozen native image identity
and expected ownership labels before issuing `start`. Backend creation also
requires its fresh boot label. A mismatch retains the failed candidate and
refuses startup; it does not delete native state to hide uncertainty. Podman's
config-ID selector and `--pull=never` behavior are unchanged.

Producer-format fixtures and controlled external-runtime regressions establish
parsing, archive/name-race refusal and command ordering. The opt-in native tests
below additionally exercise actual export and create/start primitives. Neither
category establishes selected-owner recovery, cross-runtime mounts or
desktop-focus acceptance. Those controls need their own tested revision,
invocation, complete output and exit receipt. No service or worker was changed
by the read-only fixture capture.

## Native primitive checks

Use an explicitly selected, already-loaded image and its independently recorded
configuration digest. These ignored tests require the installed Apple CLI and
`skopeo`; ordinary portable runs do not establish their results.

```bash
export WRIX_NATIVE_APPLE_IDENTITY_IMAGE=<loaded-reference>
export WRIX_NATIVE_APPLE_IDENTITY_CONFIG=sha256:<expected-configuration>
cargo test -p wrix-sandbox --lib \
  image::apple::native_test::native_export_binds_exact_index_configuration_and_reference_lookup \
  -- --ignored --exact --nocapture
cargo test -p wrix-service --lib \
  lifecycle::apple::native_test::native_stopped_apple_identity_refusal_precedes_start_and_owned_cleanup \
  -- --ignored --exact --nocapture
```

The export check uses fixture-owned temporary archives and receipt directories,
checks the actual stored-reference adapter, and verifies cold and warm receipts
without writing the operator cache. The startup check creates one uniquely
labelled container with no network or host mounts, refuses wrong-image and
wrong-owner startup while it remains stopped, then starts the bound `/bin/true`
fixture and verifies owned removal. If ownership cannot be proved, it retains
the uncertain container rather than removing another resource. Neither check
starts or upgrades the service deployment.
