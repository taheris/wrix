# Private native Nix images

## API and capability discovery

Use the existing constructor, not a separate native constructor:

```nix
let
  policy = { kind = "private-native-nix"; version = 1; };
  versions = wrixLib.capabilities.imagePolicies.private-native-nix or [ ];
in
if !builtins.elem policy.version versions then
  throw "Loom requires Wrix private-native-nix image policy v1; update Wrix"
else
  wrixLib.mkSandbox {
    profile = wrixLib.profiles.rust;
    agent = "pi";
    imagePolicy = policy;
    packages = [ loomBin ];
  }
```

The capability is exported by `lib/default.nix` and
`legacyPackages.<system>.lib`. Both `kind` and integer `version` are required
when specifying a policy. Unknown kinds, versions, fields and wrong types are
evaluation errors, not ignored options. The omitted policy is exactly
`{ kind = "default"; version = 1; }` and preserves existing behavior/identities.

V1 provisions the existing image `wrix` identity, UID/GID **1000:1000**. It is
an image-construction policy, not a runtime UID selector. Launch it as that
identity. It does not provision arbitrary UIDs or provide a shared/multi-user
Nix service. Rootless container-root users should retain the default policy.

## Loom migration

Inside Loom's `mkSandboxForRuntime`, keep one constructor call:

```nix
let
  native = profileName == "rust";
  versions = wrixLib.capabilities.imagePolicies.private-native-nix or [ ];
in
if native && !builtins.elem 1 versions then
  throw "Loom Rust workers require Wrix private-native-nix image policy v1"
else
  wrixLib.mkSandbox (
    existingSandboxArgs
    // imagePkgs.lib.optionalAttrs native {
      imagePolicy = { kind = "private-native-nix"; version = 1; };
    }
  )
```

`existingSandboxArgs` retains Loom's checked `agentPkg`, worker profile,
packages and runtime selection. Keep native-preflight metadata and the preflight
itself unchanged. Remove the `mkNativeSandbox` requirement/constructor dispatch
and the `wrix-nix-state-ancestors.patch` from patched-Wrix provisioning **in the
same migration**. Do not keep an unpatched-library fallback after the capability
check. Update Wrix to a revision advertising this capability, regenerate the
manifest, and launch its newly generated `profile_config`/source/digest.

The Wrix checkout's pinned Loom factory is still
`e353e043c427d07dd5ed9de1f330bd53109b82d3`, whose API has no policy argument.
`modules/flake/packages.nix` therefore uses a small constructor-argument adapter
at that factory boundary: Rust receives the explicit policy; base/Python receive
unchanged arguments. This is not a provisioning implementation or another
constructor. Once Loom takes the argument itself, remove that adapter and pass
`wrixLib = wrix` directly. This checkout has neither the cited Loom
compatibility patch path nor commit `1ef3ea70`; their actual upstream retirement
is not claimed as part of this Wrix change.

## Construction and preservation

`lib/sandbox/image-policy.nix` owns the typed/versioned policy schema.
`initialize-private-nix.sh` runs only in dockerTools' fakeroot customization
phase, before either the archive or OCI source is emitted. It is not copied to
an entrypoint or executed against a running store. It rejects `/` and unmarked
roots.

All three state ancestors remain root-owned `0755`. The writable store, secure
`builds` directory, per-user roots, database, temporary roots, logs and a
pre-created `gc.lock` belong to UID/GID 1000 as specified in
[the image contract](../specs/image-builder.md#image-construction-policy).
Database/WAL/store/lock contents and existing roots are not reinitialized or
removed. The common Nix configuration still selects a private local, single-user
store with signature requirements; no daemon, remote-store env or
`/homeless-shelter` guard occupation is introduced.

The policy's marker is part of the registered materialized closure and its label
enters the OCI configuration. Both source and config-digest identities change,
flowing into the generated ProfileConfig. The universal, stable-profile and
agent tiers remain reusable. Default images have no marker/label and retain
their original derivations.

## Regression commands

```sh
nix build --no-link \
  .#checks.x86_64-linux.image-policy-api \
  .#checks.x86_64-linux.image-policy-state-preservation \
  .#checks.x86_64-linux.image-policy-artifacts
nix build .#profile-images --no-link --print-out-paths
nix run .#legacyPackages.x86_64-linux.ciApps.test-loom-native-readiness
```

The first three passed: supported/rejected policies, implicit/explicit default
identity, changed source/digest/ProfileConfig identities, unchanged lower tiers,
repeated initialization preserving existing bytes/inodes, and real emitted OCI
and Docker archive ownership/modes/markers. The artifact check also realizes
both archive digest outputs and compares actual generated profile JSON.

The live runner imports the **current factory's OCI layout**, checks its loaded
config ID against the factory digest, and creates a fresh UID-1000 container by
that digest. It does not select a mutable `latest` tag or reuse an existing
container. `native-nix-operations.sh` checks the marker, completes Loom's
checked Pi wrapper/preflight, requires a successful actual Pi RPC `get_state`
response, builds and validates a new Nix output, exports/imports a previously
absent path from a separate fixture store, runs `nix develop` with signed
substitutions, and confirms all initial valid paths remain registered. Fixture
store cleanup is left to ephemeral container removal, not live store
deletion/repair.

## Fresh-image evidence (x86_64-linux)

- Manifest: `/nix/store/x4n2bw9aaw8mhif3bl0fnw3w6fcv8qsf-profile-images.json`
- Descriptor:
  `/nix/store/5qdbyjpzdd5zw619m28vrh97lir2icsl-wrix-rust-pi-nix-descriptor.json`
- OCI layout: `/nix/store/2wy20csbn3bnn0ls6zlzx10vnw88rwgh-wrix-rust-pi-oci`
- OCI config ID:
  `sha256:d67774f5f9870da1c4ffcc48d27e2df000cf75de230ca157b5d3f9917b66024d`
- ProfileConfig:
  `/nix/store/lxhddsxl5b5y2fz0rwc81czxhrvg3ypc-wrix-rust-pi-profile-config.json`
- Original unprovisioned factory image:
  `sha256:944f224af12375a7600d7f72f603928bb4d5a2867f0bac838df3e5523322d069`
- Default, non-factory `image-rust-pi.drvPath` before **and** after this change:
  `/nix/store/pzaiihdrf8fizm02qrmqjl2bys4pxxjh-wrix-rust-pi-nix-descriptor.json.drv`

Build output was re-read and the loaded Podman image inspected to confirm the
same new config ID, rather than accepting a cached old image/ref. Caching
unchanged Nix packages/Nextest and lower layers does not constitute stale-image
selection; the newly emitted policy layer and digest were selected explicitly.

This session is itself a rootless container. Ordinary nested Podman cannot mount
`/proc`, and its default single-ID mapping cannot run UID 1000. A diagnostic
user/mount namespace with the parent's two valid UID/GID ranges mapped
separately (`0 0 1`, `1 1 65536`) retained actual UID 1000. Podman 5.8.9 ran
inside that namespace with a separate overlay store. The parent sandbox's
`/proc` and `/dev` were bound, and UTS/network shared, solely for this nested
image test:

```sh
# P is Podman inside the diagnostic user/mount namespace, with:
# --storage-driver=overlay --root /workspace/.loom/scratch/wx-zip8a-podman
# --runroot /tmp/wrix-policy-podman
P pull --signature-policy <local-OCI-import-policy> oci:<layout>:latest
P image inspect --format '{{.Id}}' <config-id>
P run --rm --cgroups=disabled --network=host --uts=host \
  --mount type=bind,source=/proc,destination=/proc,ro \
  --mount type=bind,source=/dev,destination=/dev \
  --user=1000:1000 --workdir=/tmp --entrypoint /bin/bash \
  --mount type=bind,source=<pinned-nixpkgs-source>,destination=/fixture/nixpkgs,ro \
  --mount type=bind,source=<checkout>/tests/sandbox/native-nix-operations.sh,destination=/fixture/probe.sh,ro \
  -e NIX_CONFIG <config-id> /fixture/probe.sh /fixture/nixpkgs
```

Exit **0**: runtime UID 1000; complete native preflight; native Cargo build,
both Clippy passes and Nextest's real native test; actual Pi RPC response
`type=response`, `command=get_state`, `success=true`, object state data;
additive Nix build and registration, new-path import, signed cache.nixos.org
substitutions and `nix develop`; initial state preserved. The invalid-path
message before import is the intentional absence assertion, not a skipped
operation. No model request/credentials were needed for `get_state`.

These adaptations do not prove production Wrix networking/entrypoint/service
startup or Darwin runtime behavior. A normal-host run of the packaged live
runner and full `loom loop` remain unverified. The earlier Nextest rebuild
timeout was not counted as acceptance; the successful run above reached the
sandbox and executed Nextest. Capability skips are not successful verification.

Final repository verification passed on x86_64-linux: `nix fmt`, `nix build`,
`nix flake check`, `cargo build`,
`cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo nextest run`. Nextest reported 537 passed and 9 skipped; the skips are
not additional runtime proof. Nix omitted aarch64-linux and aarch64-darwin
checks.
