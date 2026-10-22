# Private Nix in every sandbox

Private, single-user Nix is an invariant of `mkSandbox`, not an optional image
policy. This capability remains distinct from the independently admitted
[shared store mode](../specs/store.md); private capability discovery does not
assert shared-mode readiness. The consumer chooses its toolchain and agent:

```nix
wrixLib.mkSandbox {
  profile = wrixLib.profiles.rust;
  agent = "pi";
}
```

Wrix owns fresh image provisioning and the matching execution identity. There is
no UID argument, permission preset, native constructor or factory adapter.

## Discovery and Loom

A consumer requiring this contract can check it before constructing images:

```nix
if (wrixLib.capabilities.privateNix or null) != 1 then
  throw "Loom requires Wrix private Nix v1; update Wrix"
else
  wrixLib.mkSandbox {
    profile = wrixLib.profiles.rust;
    agent = "pi";
  }
```

This is discovery, not a version selector or an older-constructor fallback. The
same constructor is used for every profile/runtime pair. Wrix's packaged Loom
factory receives `wrixLib` directly, without wrapping `mkSandbox` or
special-casing Rust permissions.

Consumers must delete any `mkNativeSandbox` dispatch, `imagePolicy` arguments,
private ancestor patch, and associated compatibility logic. Do not translate
those into another override. Rebuild the images and their generated
`ProfileConfig`s, and update the host Wrix executable together. The launcher
rejects images lacking the current private-Nix contract label before starting
the sandbox, including cached images and per-launch image-source overrides.

The independent `wrixLib.capabilities.sharedNix` value describes the shared
image-input contract. Images expose `image.sharedNix` (a version and immutable
`seed_closure` path list), also emitted as `ProfileConfig.image.shared_nix` and
in Linux descriptor metadata. The `wrix.nix.shared` label and
`/etc/wrix/shared-nix-version` marker agree with that version; the image's
`wrix.nix.seed` label commits to the immutable seed-list path. The list covers
exactly the materialized image store, including references found only in agent
or MCP configuration. It is input to trusted seeding, not a database to restore.
This metadata neither selects shared mode nor certifies any runtime transport;
shared launch still requires independent host approval and service/runtime
acceptance. Private construction and startup remain the default.

## Installed-store selection

Run accepts `--nix-store private|shared` in its launcher-option prefix. Spawn
accepts `nix_store.mode`; `wrix.toml` accepts
`[nix_store] mode = "private"|"shared"`. Explicit choice wins over repository
policy, then the default is `private`. Invalid owned fields still fail under an
override. This choice is independent of archive caching and
`--cache`/`--no-cache` tool-cache reuse; no environment variable selects it.

`--dry-run --json` reports the resolved choice and admission blockers without
preparing a pool. **Shared execution is unavailable in this version.** Every
shared launch fails before services, credentials, execution metadata or worker
startup; it never silently falls back. Image metadata and repository settings
are not approval. `wrix service nix-store` passively reports the selected
repository's recorded approval. `enable` requires an existing host deployment
and explicitly binds the checked namespace to a host-created canonical
repository identity; `disable` withdraws new admission for that repository's
bound namespaces without deleting data. Linked checkouts or admitted clones
share that identity only through these host bindings, never from Git paths or
repository preferences alone. Integrated scope uses the original admitted
primary checkout, not a `.loom` path heuristic. Pool keys additionally
distinguish `x86_64-linux` and `aarch64-linux`.

Approval remains metadata, not a serving capability. Native pool ownership,
strict build isolation, authenticated exports, live protection, installed reuse
and collection remain unavailable. Unknown/inconsistent approval or replaced
repository administration requires explicit reconciliation, not repair by
ordinary enable/start. No new SQLite schema or filesystem-authority fallback is
introduced.

## Construction and execution

`lib/sandbox/private-nix.nix` owns the contract version and construction-side
validation. `initialize-private-nix.sh` operates only on a marked staged image
tree under dockerTools' fakeroot. No runtime store repair, recursive live
chmod/chown, database replacement, or preflight relaxation is performed.
Existing store contents, DB/WAL bytes, GC roots and logs survive provisioning.

The `/nix`, `/nix/var`, and `/nix/var/nix` ancestors are root-owned `0755`. The
private store and secure builds directory are writable by the image's `wrix`
account; the DB, temporary roots and pre-created GC lock are accessible without
a world-writable state ancestor. `/homeless-shelter` remains absent. Build users
are disabled, signed inputs remain required, and no daemon socket or host store
is exposed.

Normal Wrix execution preserves real numeric root ownership: rootless
container-root on Linux, guest root in krun and Apple Container VMs. The image
sets `IS_SANDBOX=1`; no UID-spoofing preload, Darwin passwd rewrite or extra
user-namespace remapping is used. Rootless/VM isolation remains the security
boundary; the in-guest root identity is not host root. Fresh images also support
real UID/GID-1000 local-store operations when exercised directly, independently
of Wrix's privileged network bootstrap.

Mounts and writable directories cannot cover Nix, its configuration, the purity
guard or trusted startup paths. Caller-supplied environment/settings/secrets
cannot redirect Nix's store, daemon or configuration. Wrix's internally
generated signed project-cache configuration remains supported. The trusted
network bootstrap validates the contract marker and protected ancestors, without
repairing them, before workspace-controlled setup. The NET_ADMIN drop and
stage-two capability checks are unchanged.

The contract marker and label participate in archive and nix-descriptor image
source/config-digest identities, and consequently generated ProfileConfigs.
ProfileConfig remains one current data format; there is no old-layout mode.

## Verification

```sh
nix build --no-link \
  .#checks.x86_64-linux.private-nix-api \
  .#checks.x86_64-linux.private-nix-state-preservation \
  .#legacyPackages.x86_64-linux.ciChecks.private-nix-artifacts
nix run .#legacyPackages.x86_64-linux.ciApps.test-loom-native-readiness
```

API and preservation checks are lightweight. Artifact realization and live
container acceptance belong to the CI-only tier. The artifact verifier checks
actual OCI metadata for all three factory profiles, archive metadata, and
source/digest/ProfileConfig agreement. The live runner uses a fresh,
factory-produced Rust/Pi image as actual UID 1000: complete Loom preflight, Pi
RPC `get_state`, additive build, new-path import, signed substitution,
`nix develop`, Rust build/Clippy/Nextest and initial-path preservation.

Observed for `wx-qhq7b`: all six project verification commands passed (Cargo
Nextest: 541 passed, 9 skipped); the artifact/preservation checks passed; fresh
UID-1000 acceptance passed in a diagnostic namespace with explicit UID/GID
mappings. A separate real-root diagnostic passed the actual image network
bootstrap, NET_ADMIN drop and offline Nix evaluation/build/import with no lost
initial paths. Real firewall/capability tests passed in disposable network
namespaces for both init paths and every agent.

The accepted factory Rust/Pi image has config digest
`sha256:41ea1113f997f052bae3578fd27e53e8cdae4852383d6e70f0ec5c49769f78c2`,
descriptor
`/nix/store/smk6hdfawb5rd0szfkdcksajmz3w6fy6-wrix-rust-pi-nix-descriptor.json`,
and ProfileConfig
`/nix/store/iyqjj9q7rf6vf8369har6ffzlsklqfxr-wrix-rust-pi-profile-config.json`.
The unmodified packaged live runner was blocked in this nested harness because
its parent namespace could not map UID 1000; that failure is not a pass.

Nested-container diagnostic execution does **not** establish production Wrix
startup/network isolation, a complete `loom loop`, krun execution or Darwin
acceptance. The Darwin ownership test now goes through the real image entrypoint
and checks guest ownership, host-visible workspace writes and additive Nix.
Those platform/runtime checks still require suitable hosts. Skips, timeouts,
manifest assertions and successful image builds do not count as live acceptance.
