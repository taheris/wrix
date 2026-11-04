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
Subsequent configuration lookup is pinned to the observed native ID, not a
second independent mutable-name observation.

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
parsing, archive/name-race refusal and command ordering. They are not native
create/start, selected-owner recovery, cross-runtime mount or desktop-focus
acceptance. Those controls need their own tested revision, invocation, complete
output and exit receipt. No service or worker was changed by the read-only
fixture capture.
