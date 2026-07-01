# OCI Image Source Builder

Nix expression and descriptor builder that turn wrix-managed images into OCI image sources consumed by platform launchers.

## Problem Statement

Sandboxes need container images with all profile packages pre-installed, an agent runtime configured and ready, Nix configured for in-container package management, CA certificates for HTTPS, and the platform-specific entrypoint embedded. Wrix support services need the same image-source contract so local runtime images are installed and cleaned up consistently. The profile builder produces images from a profile (`profiles.md`) plus an agent runtime axis (claude / pi / direct) without leaking platform-specific paths into the image expression; the Linux archive-less source contract applies to every Linux-consumed wrix-managed Nix-built image, while Darwin-consumed images name the tar-loadable `docker-archive` source kind explicitly.

## Architecture

`mkImage` (in `lib/sandbox/image.nix`) is the internal API called by `mkSandbox` (see `sandbox.md`). Inputs: a profile, an agent runtime selector, the resolved selected `agentPkg`, an entrypoint script path (Linux or Darwin), optional krun support, merged agent settings JSON, and the resolved MCP server configs. On Linux it emits a JSON image descriptor (`source_kind = "nix-descriptor"`) plus a prebuilt OCI layout whose layer descriptors are consumed by the runtime image installer's `skopeo oci:` install path. On Darwin it emits a Docker/OCI tar archive loadable by Apple's `container image load` path (`source_kind = "docker-archive"`). Service/support images do not necessarily use `mkImage`, but they expose the same `{ ref, source, source_kind, digest }` contract and ownership labels. The Darwin-only `wrix-builder` bootstrap image is a support image in that family: `linux-builder.md` owns its Apple `container` lifecycle and persistent-store seeding contract, while this spec owns its source-kind metadata and wrix-managed labels.

Profile image layout:

```
/
├── bin/, lib/           # Profile packages (from Nix store, layered)
├── entrypoint.sh        # Platform-specific startup script
└── etc/
    ├── nix/nix.conf     # Flakes + nix-command enabled, sandbox disabled
    └── ssl/certs/       # CA certificates for HTTPS
```

Build pipeline:

1. Collect packages from the profile (profile toolchain + agent tooling + MCP server packages if any)
2. Compose the agent tier (`wrix-agent-<agent>-<name>`) with exactly the selected `agentPkg`; a non-selected agent's binary is not present
3. Configure `/etc/nix/nix.conf` for flakes and disabled in-container sandbox (the outer container is the security boundary — see `specs/security.md`)
4. Bundle CA certificates from `pkgs.cacert`
5. Emit a platform image source with the four-tier graph described below: Linux as a descriptor graph and Darwin as a tar-loadable image, with the entrypoint script as the OCI command

`mkImageRef` (also in `lib/sandbox/`) produces the stable platform ref the launcher expects (`localhost/<name>:<hash-tag>` on Linux, bare `<name>:<hash-tag>` on Darwin's Apple `container` CLI). `mkSandbox` records both the source path and explicit source kind in `ProfileConfig` so launchers and orchestrators do not infer transport semantics from filenames or host platform alone. Other wrix-managed Nix-built images use the same source-kind distinction on Linux and Darwin.

Source kinds are stable API values:

| `source_kind` | Meaning |
|---------------|---------|
| `nix-descriptor` | Linux JSON descriptor consumed by wrix; it points at a prebuilt OCI layout copied with `skopeo oci:` and is not itself a whole-image tar or stream script |
| `docker-archive` | Tar-loadable archive consumed by Darwin's `container image load --input <tar>` fallback |

## Provenance-Tiered Layering

This section describes the profile-image layer graph used by `mkImage`. Service/support images follow the source-kind, digest, and label contract above; their owning specs may define a simpler layer graph when the profile-specific tiers do not apply.

A profile image is a **four-tier graph**, each tier a derivation whose layer membership is fixed by its own closed set of contents. The fixed tier boundary is the cache contract: a store path keeps its layer assignment regardless of what changes above or below it, so per-blob transports can reuse unchanged layer digests instead of re-reading or re-tarring the same store paths.

The four tiers, bottom (most stable) to top (most volatile):

| Tier | Derivation | Contents | Changes with |
|------|-----------|----------|--------------|
| 0 — base | `wrix-base-image` | universal nixpkgs bottom-of-closure (glibc, gcc-lib, openssl, cacert, shells, coreutils) | nixpkgs pin |
| 1 — stable-profile | `wrix-stable-profile-<name>` | the profile's `corePackages` (the `basePackages` floor + the profile toolchain, default *or* pinned) plus wrix-generated derivations (notify client, prek wrappers + bundle, nix.conf, passwd/group) — **no agent runtime** | wrix source, nixpkgs pin, a toolchain re-pin |
| 2 — agent | `wrix-agent-<agent>-<name>` | exactly the **one** selected `agentPkg` runtime and its closure. A non-selected agent's binary is absent | the agent selection and that agent package's own version |
| 3 — leaf | the per-profile image | downstream-appended packages (`profile.packages` − `corePackages`) and per-invocation generated content (merged agent settings, MCP configs, the `profileEnv` symlink tree, entrypoint `extraCommands`), plus the Nix DB registration file | each consumer iteration |

Linux represents tiers as OCI layer descriptors plus a final JSON image descriptor. The descriptor records the OCI layout path, image ref, config digest, layer digest list, layer sizes, media types, diffIDs, and the Nix store paths or generated rootfs content that produced the blobs. The selected Linux source path is the descriptor JSON, not the OCI layout directory, a Docker archive, an OCI archive, or a stream script.

Darwin keeps the tar-loadable path until an Apple-compatible per-blob install path is verified. Its build output may continue to use `dockerTools.buildLayeredImage`/`fromImage` internally, but the tier membership rule is the same as Linux: lower-tier contents are not mixed into higher-tier membership, and volatile leaf metadata is isolated from package closures.

**Tier membership rule.** Three discriminators separate the three non-base tiers. *Fixed-per-profile-instance* content (tier 1) is the profile's `corePackages` plus the wrix-generated derivations; the profile exposes it through `corePackages` (see `profiles.md` § Profile Attrset Schema), the wrix-controlled set `mkProfile` fixes at construction — downstream extension (`deriveProfile`, `rustProfile { packages }`) appends only to `profile.packages`, never to `corePackages`. The *one selected `agentPkg` runtime* is tier 2. *Varies-per-iteration* content — the appended delta `profile.packages` − `corePackages` and the volatile generated files — is tier 3 (leaf). A downstream-**pinned** toolchain (`rustProfile { toolchain = ./rust-toolchain.toml }`) is part of `corePackages` and therefore tier 1.

**Tier ordering (toolchain below agent).** The agent tier sits *above* the toolchain, not below, because a change low in the graph re-emits every tier above it, and the toolchain is by far the heaviest mutable path (a pinned rust toolchain is ~1.5 GB versus ~230 MB for `claude-code` and less for a `direct` runner). Placing the toolchain lowest — only `wrix-base-image` beneath it — means almost nothing ever drags it: it re-ships only when it itself changes. The inverse order would re-ship the whole toolchain on every agent-version bump, and for `direct` — whose runner is the consumer's own frequently-rebuilt binary — that would be on nearly every iteration. The light agent binary is the cheap thing to cascade, so it rides above; cascade cost is `freq × weight-of-tier-dragged`, and the toolchain's weight dominates regardless of relative cadence.

**Tier 0 membership.** A path belongs in `wrix-base-image` iff (a) it varies only with the nixpkgs pin, not with any profile-level input or wrix-generated content, **and** (b) it is genuinely universal — every profile already closes over it as shared bottom-of-closure, not a profile-specific leaf. Condition (a) alone is necessary but not sufficient. A pin-stable but non-universal path must not be hoisted into tier 0 merely because its hash tracks only the pin: doing so would force every profile image to carry it even where it is never closed over. A profile-specific compiler toolchain such as `pkgs.rustc` is the case in point — base and python close over no Rust at all, and even the rust profile pulls fenix's toolchain (a different store path), so `pkgs.rustc` in tier 0 would be dead weight in every image. Such a path stays in the profile-specific tier 1 domain, never tier 0.

The leaf's **customisation layer** is the final, smallest layer for generated files and metadata: merged agent settings, MCP configs, `/etc/wrix/image-agent`, Nix DB registration, entrypoint copies, and any generated symlink tree not held in a standalone Nix store path. A generated-settings or Nix-DB-only change must change only the image descriptor/config and this top customisation layer; it must not re-hash tier-0, tier-1, tier-2, or downstream package-delta blobs, nor copy lower-tier store paths into a higher tier. The `profileEnv` symlink tree references tier-0, tier-1, and tier-2 store paths that resolve from lower layers at runtime, so higher tiers strip lower-tier paths from their own graph without breaking PATH. The 127-layer OCI ceiling splits across the fixed tiers and the leaf delta rather than forcing unrelated paths into a single volatile layer.

## Linux Archive-Less Install Surface

On Linux, the selected image source path (`ProfileConfig.image.source` for profile images, or the equivalent service/support image metadata) is a JSON descriptor, not a Docker archive, OCI archive, raw OCI layout path, or stream script. The descriptor includes `source_kind = "nix-descriptor"`, the config digest used for install preflight, the OCI manifest/config/layer descriptors, and an `oci_layout` store path plus `oci_ref`. The runtime image installer validates the source kind, reads the descriptor, and copies `oci:<oci_layout>:<oci_ref>` directly into `containers-storage:<ref>` with skopeo. This avoids the legacy whole-image tar and stream-script transports; it does not specify a custom per-blob `nix:` transport.

The image digest used for install preflight is derived from descriptor/config metadata. Computing it must not execute the image source, materialize a whole-image tar, or run a Docker-archive-to-OCI conversion. When the destination already has that digest, `wrix run` and `wrix spawn` skip before invoking the descriptor's install source. On a digest miss, the OCI layout may be copied by skopeo, while the provenance-tiered graph still guarantees unchanged lower-tier layer digests stay byte-identical. The descriptor remains reproducible: given the same profile, selected agent, agent package, generated settings, and MCP config, it produces the same layer digest list and config digest.

## Image Ownership Labels

Every wrix-managed image carries labels that identify it as wrix-owned. At minimum, all such images include `wrix.managed=true` and `wrix.image.kind=<kind>`. Profile images use `wrix.image.kind=profile` and additionally include `wrix.profile.name=<name>` plus `wrix.agent.kind=<direct|claude|pi>`. Service images use `wrix.image.kind=service`; other support images include their own kind-specific labels as named by their specs. Wrix does not relabel third-party/user-provided images, and automatic cleanup may target only images that are labelled wrix-managed or legacy refs explicitly named by the cleanup spec. Cleanup policy and runtime image retention live in `sandbox.md` and sibling runtime specs; this spec owns only that image outputs expose enough metadata for safe wrix-scoped cleanup.

## In-Container Nix Store Consistency

The container runs Nix as the runtime user directly against the store — no
`nix-daemon`, and the in-container build sandbox is disabled (Build pipeline
step 3; threat-model rationale in `specs/security.md`). On the default boundary
that user is rootless container-root, which
maps to the host user that owns the baked store, so it can both *add* paths
(substituting from a binary cache, building new derivations) and *mutate* the
baked root-owned paths (replace, GC, delete) — store ownership is no longer a
write barrier (runtime acceptance owned by `sandbox.md`).

The image must still ship a **Nix database that exactly matches its on-disk
store** — the registered-valid set and the on-disk `/nix/store` contents are
the *same* closure-closed set, with no discrepancy in **either** direction:

- **No dangling registration** (registered valid but absent on disk) — the
  load-bearing correctness requirement. Nix trusts the DB and feeds the missing
  path into a build as though present; the builder then fails with `No such
  file or directory`. Because such a path is typically a build intermediate
  with `allowSubstitutes = false`, it cannot self-heal by substitution either,
  and the container cannot always rebuild it locally — so the fault is
  unrecoverable without manual store surgery.
- **No orphan** (on-disk but unregistered). No longer a correctness failure now
  that the runtime user owns the store — Nix treats the path as missing,
  deletes the stale on-disk copy (a `chmod` the store owner can now perform),
  and rebuilds it. Registering over exactly the materialized contents avoids
  the wasted rebuild for free.

The registration is therefore derived from the **materialized contents
closure** — the exact path set copied into the image layers — **not** the
build derivation's full closure. The full build closure drags in unmaterialized
intermediates (the `wrix-*-profile-env` buildEnv and image-build artifacts
such as the customisation-layer tar and `layering.json`) that are never copied
into the rootfs; registering them is precisely what bakes a dangling path.
Computing the registration (`closureInfo` over the materialized path list +
`nix-store --load-db`) from that same list keeps the DB an honest description
of the disk.

This is also the cache-optimal choice: registering exactly what is already
materialized copies up **no** new store path, so every tier-0, tier-1, and
tier-2 blob stays byte-identical across rebuilds and only the single DB file
in the leaf's customisation layer re-hashes, at that layer's existing cadence —
the provenance-tiered graph is unaffected. Materializing the missing
intermediates into the rootfs instead would add store-path copy-up and re-hash
a lower tier, trading the consistency bug for a caching regression.

The runtime acceptance — a fresh container letting the runtime user run
`nix develop` / `nix build` and mutate baked store paths — is owned by
`sandbox.md`.

## Hook Installation

Every profile image carries the prek surfaces defined by `pre-commit.md` § Hook Installation in Profile Containers so commits and pushes from inside the container fire through the same hook bundle as the host. This spec owns the image-builder side of that contract:

- the `wrix.prekHooks` bundle is materialized in the image closure and exposed to the entrypoint as the hook path;
- the wrapper binaries defined by `pre-commit.md` § Hook-Entry Wrappers are available on the profile image `PATH`;
- the platform entrypoint (`lib/sandbox/{linux,darwin}/entrypoint.sh`) sets `core.hooksPath` on `/workspace/.git` to the `wrix.prekHooks` store path when `.pre-commit-config.yaml` is present, mirroring `mkDevShell`'s host-side step.

Wrapper behavior, hook-stage semantics, and optional-tool policy remain owned by `pre-commit.md`. The image builder does not inject `SKIP=` env vars, stub missing tools on `PATH`, or maintain a hook-id skip list.

## Success Criteria

- The runtime image installer (under both `wrix run` and `wrix spawn`) short-circuits when the image's content digest is already present in the platform store: no source execution, no tar materialization, no stream invocation, no `*-load` CLI call
  [system](bash tests/test-app.sh test-image-install-digest-skip)
- On Linux, `mkImage` emits a JSON image descriptor (`source_kind = "nix-descriptor"`) as the selected source path; the descriptor points at a prebuilt OCI layout but is not itself the raw image output, a Docker archive, an OCI archive, or a stream script
  [system](bash tests/test-app.sh test-linux-image-archiveless-source)
- The Linux image digest used for install preflight is computed from descriptor/config metadata without executing the image source, materializing a whole-image tar, or running Docker-archive-to-OCI conversion
  [system](bash tests/test-app.sh test-image-digest-no-tar)
- Each profile image is a four-tier graph — leaf atop `wrix-agent-<agent>-<name>`, atop `wrix-stable-profile-<name>`, atop `wrix-base-image` — with Linux represented as descriptor layers and Darwin represented as a tar-loadable fallback
  [system](bash tests/test-app.sh test-image-tier-graph)
- A deterministic layer-membership verifier proves each non-base profile-image tier removes or skips the union of all lower tiers' closures, so no tier re-emits a store path a lower tier already ships
  [system](bash tests/test-app.sh test-image-tier-membership)
- `wrix-base-image`'s derivation hash is invariant under changes to profile-level inputs — `profile.packages`, `profile.env`, MCP configs, the merged Claude settings JSON, and the agent runtime selection
  [system](bash tests/test-app.sh test-base-image-hash-stable)
- `wrix-base-image` holds only the universal bottom-of-closure: no profile-specific compiler toolchain leaks in (e.g. `pkgs.rustc`, which no profile references — the rust profile uses fenix's toolchain)
  [system](bash tests/test-app.sh test-base-image-universal)
- `wrix-stable-profile-<name>`'s derivation hash is invariant under the agent runtime selection and all leaf (tier-3) inputs — downstream-appended packages (`profile.packages` − `corePackages`), the merged agent settings JSON, and MCP configs
  [system](bash tests/test-app.sh test-stable-profile-hash-stable)
- `wrix-stable-profile-<name>` holds only fixed-per-instance content: no agent runtime and no downstream-appended package leaks into it
  [system](bash tests/test-app.sh test-stable-profile-membership)
- A downstream-pinned toolchain (`rustProfile { toolchain = ./rust-toolchain.toml }`) lands in tier 1 (`wrix-stable-profile-<name>`), not the leaf
  [system](bash tests/test-app.sh test-pinned-toolchain-stable-tier)
- A tier-3 (leaf) change — a downstream-appended package, an agent-settings field, or an MCP-config field — leaves every tier-0, tier-1, and tier-2 (agent) layer-blob byte-identical in the resulting image's manifest; only leaf blobs change
  [system](bash tests/test-app.sh test-downstream-change-leaf-only)
- A generated-metadata-only change — agent settings, MCP config, entrypoint metadata, or Nix DB registration — changes only the image descriptor/config and the tiny top customisation layer; lower-tier layer blobs stay byte-identical and descriptor/layout builders do not depend on the raw whole-image output
  [system](bash tests/test-app.sh test-archiveless-generated-change)
- The selected agent runtime rides its own tier `wrix-agent-<agent>-<name>`, chained atop `wrix-stable-profile-<name>`; an agent-version change leaves every tier-0 and tier-1 blob byte-identical
  [system](bash tests/test-app.sh test-agent-tier-isolated)
- The leaf image declares the selected agent variant in `/etc/wrix/image-agent`, which the entrypoint uses to reject ProfileConfig/image mismatches before agent exec
  [system](bash tests/sandbox/agent-binary-guard.sh)
- A non-selected agent's binary is absent from the image: an `agent = "direct"` image contains neither `claude-code` nor a `pi` runtime
  [system](bash tests/test-app.sh test-agent-exclusive)
- default `agent = "direct"` produces an image that contains `loom-direct-runner`
  [system](bash tests/test-app.sh test-agent-exclusive)
- `agent = "claude"` produces an image that contains `claude-code`
  [system](bash tests/test-app.sh test-agent-exclusive)
- The `agentPkg` code path threads the selected agent package into the image build
  [system](bash tests/test-app.sh test-agent-pkg-threaded)
- `nix-command` and `flakes` are enabled in `/etc/nix/nix.conf` and Nix's in-container build sandbox is disabled
  [system](bash tests/test-app.sh test-image-nix-config)
- The baked image's Nix database registers no orphaned path: every path on disk under `/nix/store` is registered valid
  [system](bash tests/test-app.sh test-image-nix-db-consistent)
- The baked image's Nix database registers no dangling path: every path registered valid exists on disk, so a freshly provisioned container passes `nix-store --verify --check-contents` with zero missing paths and an additive `nix build` cannot fail with `No such file or directory` on a registered path. The registration is derived from the materialized contents closure, not the full build closure
  [system](bash tests/test-app.sh test-image-nix-db-no-dangling)
- CA certificates from `pkgs.cacert` are baked into the image and `SSL_CERT_FILE` resolves to the bundle
  [system](bash tests/test-app.sh test-image-ca-certificates)
- The platform entrypoint script (`lib/sandbox/{linux,darwin}/entrypoint.sh`) is the image's startup command
  [system](bash tests/test-app.sh test-image-entrypoint-command)
- `wrix.prekHooks`, `wrix.prePushChecks`, and `wrix.skipIfMissing` all land in every profile image's store closure
  [system](bash tests/test-app.sh test-prek-hooks-closure)
- The Linux entrypoint sets `core.hooksPath` on `/workspace/.git` to the `wrix.prekHooks` store path when `.pre-commit-config.yaml` is present
  [system](bash tests/sandbox/entrypoint-contract.sh test_linux_core_hooks_path)
- The Darwin entrypoint mirrors the Linux entrypoint's `core.hooksPath` setup for `/workspace/.git`
  [system](bash tests/sandbox/entrypoint-contract.sh test_darwin_core_hooks_path)
- Every wrix-managed Nix-built image source covered by this spec exposes the platform source kind (`nix-descriptor` on Linux, `docker-archive` on Darwin), including service/support images such as `wrix-builder`
  [system](bash tests/test-app.sh test-wrix-images-source-kind)
- Wrix-managed images carry wrix-managed image labels, including `wrix.managed=true` and `wrix.image.kind`; profile images also carry `wrix.profile.name` and `wrix.agent.kind`
  [system](bash tests/test-app.sh test-wrix-image-labels)

## Requirements

### Functional

1. **OCI image generation** — `mkImage` returns a platform image source: a Linux descriptor consumed by wrix's `skopeo oci:` install path, and a Darwin tar archive consumed by `container image load`.
2. **Package bundling** — every derivation in the profile's `packages` list lands in the image's store closure.
3. **Agent runtime composition** — the `agent` parameter selects the single agent runtime the image carries: `direct` (default placeholder runner), `claude` (`claude-code` from nixpkgs), or `pi` (`pi-coding-agent` from nixpkgs by default). `agentPkg` overrides the selected runtime package. Exactly one agent is baked — a non-selected agent's binary is absent (a `direct` image carries no `claude-code`) — and it rides its own tier (tier 2, see § Provenance-Tiered Layering), composing orthogonally with the workspace profile.
4. **Nix configuration** — `flakes` and `nix-command` are enabled; the in-container Nix sandbox is disabled (the outer container is the boundary).
5. **CA certificates** — `pkgs.cacert` is included and `SSL_CERT_FILE` resolves to it.
6. **Entrypoint embedding** — the platform-specific entrypoint script (`lib/sandbox/{linux,darwin}/entrypoint.sh`) is the image's startup command.
7. **Hook installation** — every profile image carries `wrix.prekHooks` plus `wrix.prePushChecks` and `wrix.skipIfMissing` on `PATH`; the entrypoint configures `core.hooksPath` on `/workspace/.git` when `.pre-commit-config.yaml` is present. See `pre-commit.md` for the wrapper contracts.
8. **Profile-image provenance-tiered layering** — each profile image is a four-tier graph (base → stable-profile → agent → leaf). Each tier's layer membership is fixed by its own closed contents and removes or skips the union of all lower tiers' closures, so a tier-3 (leaf) change leaves tier-0, tier-1, and tier-2 blobs byte-identical. Tier membership: the wrix floor + profile toolchain + wrix-generated content is tier 1 (keyed on `corePackages`, see `profiles.md`); the single selected `agentPkg` runtime is tier 2; downstream-appended packages and per-invocation generated files are tier 3. The agent tier rides *above* the toolchain so an agent-version bump never re-ships the heavier toolchain (which sits lowest, dragged only by a base change). Linux represents the graph as a descriptor plus OCI layer metadata; Darwin may represent it as a tar-loadable `fromImage` chain until Darwin per-blob install is verified. Service/support images follow the same source-kind contract but may define their own layer graph.
9. **Linux descriptor source** — the Linux source artifact is a small descriptor containing the ordered layer descriptors, config metadata, and a prebuilt OCI layout reference. It is not a Docker archive, OCI archive, raw OCI layout path, or stream script. Digest-hit installs skip before invoking skopeo; digest-miss installs copy the descriptor's OCI layout into containers-storage.
10. **Store/DB consistency** — the image's Nix database registers *exactly* its on-disk contents closure: no orphaned (on-disk but unregistered) path and no dangling (registered but absent) path, in either direction. The registered set is derived from the materialized path list copied into the image layers, not the build derivation's full closure (which includes unmaterialized intermediates like the `wrix-*-profile-env` buildEnv). This prevents a build from trusting a registered-but-missing path (`No such file or directory`, unrecoverable without store surgery); the no-orphan direction is no longer a correctness requirement now that the runtime user owns the store, but registering over exactly the materialized contents secures it for free. The registration rides as a single DB file in the leaf customisation layer — it copies up no store path and does not perturb the provenance-tiered graph. Runtime acceptance is owned by `sandbox.md`.
11. **Image labels** — wrix-managed images carry labels so cleanup can distinguish wrix-owned images from user images. All such images include `wrix.managed=true` and `wrix.image.kind=<kind>`; profile images use `wrix.image.kind=profile` and include `wrix.profile.name` plus `wrix.agent.kind`; service/support images include kind-specific identity labels. Third-party/user-provided images are not relabelled by wrix and are not eligible for automatic wrix cleanup unless they already carry wrix-managed labels or match an explicitly documented legacy ref rule.

### Non-Functional

1. **Layered for caching** — the provenance-tiered graph (see § Provenance-Tiered Layering) fixes each tier's layer membership by construction, so a change in one tier does not re-hash a lower tier's blobs.
2. **Reproducible** — same profile, agent selector, and consumer-supplied agent package (when applicable) produce the same image hash; `mkImageRef` is a pure function of the image.
3. **Iteration cost bounded by change size** — a change at tier 3 (a downstream-appended package, one MCP-config field, one entry in the agent settings JSON) re-emits only the leaf's delta; tier-0, tier-1, and tier-2 (agent) blobs stay byte-identical. On Linux, descriptor metadata and OCI layer digests isolate unchanged lower tiers from higher-tier re-hashing; digest-hit installs skip source copying before skopeo runs. An agent-version bump re-emits only the agent tier and the leaf, leaving the heavier tier-0/tier-1 blobs byte-identical. Darwin remains archive-load based until a per-blob install path is verified (see *Out of Scope*).

## Out of Scope

- Multi-architecture manifests (arm64 + amd64 in a single ref)
- Image signing
- Registry push automation (consumers install from a Nix store path via the runtime image installer's platform install path; remote registries are user-side concerns)
- Per-layer-blob-dedup install on Darwin — Apple's `container` CLI exposes only `container image load --input <tar>` for local archive import; neither a `skopeo` transport into Apple's container store nor an Apple-native per-blob-dedup install primitive has been verified. Darwin keeps the tar/load fallback and content-digest install-skip preflight. Promoting Darwin to archive-less or per-blob-dedup install requires a verified transport into the Apple `container` store.
