# Native Apple image producer fixture

Captured read-only on macOS 27.2 with Apple `container` CLI 1.5.0
(`build: release, commit: unknown`), while Wrix checkout revision was
`0b178828`. The existing image was `wrix-service:e5cbefb0`; this capture does
not establish its original source revision or migrate its schema-1 owner.

Commands completed with exit 0:

```bash
container image inspect wrix-service:e5cbefb0
container image save --platform linux/arm64 --output service-e5.oci.tar wrix-service:e5cbefb0
skopeo inspect --raw oci-archive:service-e5.oci.tar
skopeo --override-os linux --override-arch arm64 inspect --config --raw oci-archive:service-e5.oci.tar
```

`inspect.json` is the complete inspection result (JSON whitespace reformatted).
The `.blob` files are exact bytes extracted from the exported OCI archive; they
must not be JSON-reformatted:

- `native-index.blob`:
  `sha256:e5a9e301485cdd81c93c4d07f6fa76481b182594fbd9d15756a06e2ce6ecc533`
- Variant manifest:
  `sha256:abc267b76505c7f114e027fd10e25c60c8698c1274c8b5395607ba42b3e8355e`
- `native-config.blob`:
  `sha256:167ed5405630356632ef31e8421fb0494a2804f2f47bfe35874d2909efcf549d`

All three identities differ. `inspect --config` without `--raw` reformats the
configuration and does not preserve its byte digest. The CLI's recorded Nix
build source was also reviewed: `ImageResource.Variant.digest` is a manifest
digest, and `ImageResource.id` comes from the index descriptor. Neither is the
configuration digest.

This is producer-format conformance evidence, not native create/inspect/start,
mutable-name-race execution, running-owner recovery or desktop acceptance. No
service, worker, owner metadata, key or native reference was changed by the
capture.
