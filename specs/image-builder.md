# OCI Image Builder

Nix expression that turns a wrapix profile into an OCI container image consumed by the platform launchers in `sandbox.md`.

## Problem Statement

Sandboxes need container images with all profile packages pre-installed, an agent runtime configured and ready, Nix configured for in-container package management, CA certificates for HTTPS, and the platform-specific entrypoint embedded. The builder produces these from a profile (`profiles.md`) plus an agent runtime axis (claude / pi / direct) without leaking platform-specific paths into the image expression.

## Architecture

`mkImage` (in `lib/sandbox/image.nix`) is the internal API called by `mkSandbox` (see `sandbox.md`). Inputs: a profile, an agent runtime selector, an entrypoint script path (Linux or Darwin), optional krun support, the merged Claude settings JSON, and the resolved MCP server configs. When `agent` is `"pi"` or `"direct"`, the caller also supplies the agent package (`piPkg` or `directRunner` respectively); `mkSandbox` throws when it's missing. It emits a layered OCI image via `dockerTools.streamLayeredImage` (Linux) or `dockerTools.buildLayeredImage` (Darwin — the stream script's Linux Python shebang cannot execute on macOS).

Image layout:

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
2. Compose the agent runtime layer (claude is a no-op; pi adds the consumer-supplied `piPkg` package; direct adds the consumer-supplied `directRunner` package)
3. Configure `/etc/nix/nix.conf` for flakes and disabled in-container sandbox (the outer container is the security boundary — see `specs/security.md`)
4. Bundle CA certificates from `pkgs.cacert`
5. Emit the leaf image atop the `wrapix-stable-profile-<name>` → `wrapix-base-image` chain via `fromImage` (see § Provenance-Tiered Layering), with the entrypoint script as the OCI `Cmd`

`mkImageRef` (also in `lib/sandbox/`) produces the stable podman ref the launcher expects (`localhost/<name>:<hash-tag>` on Linux, bare `<name>:<hash-tag>` on Darwin's Apple `container` CLI).

## Provenance-Tiered Layering

The image is a **three-stage `fromImage` chain**, each stage a derivation whose layer membership is fixed by its own closed set of contents. Chaining is the only mechanism that makes a store path's layer assignment independent of the rest of the image — the property skopeo's per-blob install transport (`sandbox.md` § Image install path) needs to dedup across rebuilds. A single image's automatic layering derives membership from the *global* path-popularity ranking, so adding or removing one input re-partitions which paths share a layer and re-hashes every affected blob (the install transport then re-transfers them instead of reusing the cache). Fixing each tier's contents eliminates that re-partition: a path keeps its layer regardless of what changes in the tiers above or below it.

The three tiers, bottom (most stable) to top (most volatile):

| Tier | Derivation | Contents | Changes with |
|------|-----------|----------|--------------|
| 0 — base | `wrapix-base-image` | universal nixpkgs bottom-of-closure (glibc, gcc-lib, openssl, cacert, shells, coreutils) | nixpkgs pin |
| 1 — stable-profile | `wrapix-stable-profile-<name>` | the profile's `corePackages` (the `basePackages` floor + the profile toolchain, default *or* pinned) plus wrapix-generated derivations (notify client, prek wrappers + bundle, default `claude-code`, nix.conf, passwd/group) | wrapix source, nixpkgs pin, a toolchain re-pin |
| 2 — leaf | the per-profile image | downstream-appended packages (`profile.packages` − `corePackages`), the agent runtime (`piPkg`/`directRunner`), and per-invocation generated content (merged Claude settings, MCP configs, the `profileEnv` symlink tree, entrypoint `extraCommands`) | each consumer iteration |

Tier 1 is built with `dockerTools.buildLayeredImage` (a tar in the store) so it can serve as the leaf's `fromImage` on both Linux and Darwin — the same tar-only constraint `base-image.nix` records for tier 0. The leaf is `streamLayeredImage` on Linux and `buildLayeredImage` on Darwin. Each non-base tier sets `fromImage` to the tier below and drives its `layeringPipeline` with `remove_paths` over the **union of all lower tiers' closures**, so no tier re-emits a path a lower tier already ships. Within a tier, standard dockerTools layering applies, but because the tier's contents are a fixed set the ordering inside it is stable — the global-ranking re-partition that destabilised the old single-image layering cannot occur.

**Tier membership rule.** The discriminator is *fixed-per-profile-instance* (tier 1) vs *varies-per-iteration* (tier 2), which the profile exposes through `corePackages` (see `profiles.md` § Profile Attrset Schema). `corePackages` is the wrapix-controlled set `mkProfile` fixes at construction; downstream extension (`deriveProfile`, `rustProfile { packages }`) appends only to `profile.packages`, never to `corePackages`. So `corePackages` ∪ the wrapix-generated derivations is tier 1, and the rest — the appended delta `profile.packages` − `corePackages`, the consumer-supplied agent runtime, and the volatile generated files — is tier 2. A downstream-**pinned** toolchain (`rustProfile { toolchain = ./rust-toolchain.toml }`) is part of `corePackages` and therefore tier 1: it is customised once but fixed for the instance, and it is the heaviest, lowest-cadence path in the image, so it belongs in a fixed-membership tier rather than churning with the variable leaf.

**Tier 0 membership.** A path belongs in `wrapix-base-image` iff (a) it varies only with the nixpkgs pin, not with any profile-level input or wrapix-generated content, **and** (b) it is genuinely universal — every profile already closes over it as shared bottom-of-closure, not a profile-specific leaf. Condition (a) alone is necessary but not sufficient. A pin-stable but non-universal path must not be hoisted into tier 0 merely because its hash tracks only the pin: doing so would force every profile image to carry it even where it is never closed over. A profile-specific compiler toolchain such as `pkgs.rustc` is the case in point — base and python close over no Rust at all, and even the rust profile pulls fenix's toolchain (a different store path), so `pkgs.rustc` in tier 0 would be dead weight in every image. Such a path stays in the profile-specific tier 1 domain, never tier 0.

The leaf's **customisation layer** (its final layer — generated files, metadata, and anything not held in a standalone Nix store path, including the `profileEnv` symlink tree) re-hashes whenever any of its inputs change, but it rides above the stable tiers, so a tier-2-only change leaves every tier-0 and tier-1 blob byte-identical. The `profileEnv` symlink tree references tier-0 and tier-1 store paths that resolve from the lower layers at runtime, so the leaf's `remove_paths` strips them from its own graph without breaking PATH. Each tier's own layer count stays well below the 127-layer OCI ceiling: tiers 0 and 1 bound `maxLayers` to their fixed closures, and the leaf budgets only its delta plus the customisation layer.

## Hook Installation

Every profile image carries the host-equivalent prek setup so commits and pushes from inside the container fire the same `.pre-commit-config.yaml` chain the host runs (see `pre-commit.md` § Hook Installation in Profile Containers):

- `wrapix.prekHooks`, `wrapix.prePushChecks`, and `wrapix.skipIfMissing` are wired into the image's package set — `prekHooks` so its store path is reachable by `core.hooksPath`, and the two wrappers so they resolve on `PATH` for prek `entry:` lines that reference them by name.
- The platform entrypoint (`lib/sandbox/{linux,darwin}/entrypoint.sh`) sets `core.hooksPath` on `/workspace/.git` to the `wrapix.prekHooks` store path when `.pre-commit-config.yaml` is present, mirroring `mkDevShell`'s host-side step.
- Profile images for bead use do not ship `nix` by default; nix-requiring hooks remain inert under `skip-if-missing nix --` (see `pre-commit.md`). The image builder does not inject `SKIP=` env vars, does not stub `nix` on `PATH`, and does not maintain a hook-id skip list. A profile that wants nix to fire in-container ships `pkgs.nix` via its own `packages`.

## Success Criteria

- The launcher's image-install step (under both `wrapix run` and `wrapix spawn`) is short-circuited when the image's content digest is already present in the platform store: no tar materialization, no stream invocation, no `*-load` CLI call
  [system](nix run .#test-image-install-digest-skip)
- The image is a three-stage `fromImage` chain — the leaf chains atop `wrapix-stable-profile-<name>`, which chains atop `wrapix-base-image` — on both Linux (`streamLayeredImage` leaf) and Darwin (`buildLayeredImage` leaf)
  [check?](grep -nE 'fromImage|wrapix-stable-profile|wrapix-base-image' lib/sandbox/image.nix lib/sandbox/stable-profile-image.nix)
- Each non-base tier removes the union of all lower tiers' closures from its layering graph via `remove_paths`, so no tier re-emits a store path a lower tier already ships
  [check?](grep -nE 'remove_paths' lib/sandbox/image.nix lib/sandbox/stable-profile-image.nix)
- `wrapix-base-image`'s derivation hash is invariant under changes to profile-level inputs — `profile.packages`, `profile.env`, MCP configs, the merged Claude settings JSON, and the agent runtime selection
  [system](nix run .#test-base-image-hash-stable)
- `wrapix-base-image` holds only the universal bottom-of-closure: no profile-specific compiler toolchain leaks in (e.g. `pkgs.rustc`, which no profile references — the rust profile uses fenix's toolchain)
  [system](nix run .#test-base-image-universal)
- `wrapix-stable-profile-<name>`'s derivation hash is invariant under tier-2 inputs — downstream-appended packages (`profile.packages` − `corePackages`), the agent runtime selection, the merged Claude settings JSON, and MCP configs
  [system?](nix run .#test-stable-profile-hash-stable)
- `wrapix-stable-profile-<name>` holds only fixed-per-instance content: neither a downstream-appended package nor a consumer-supplied agent runtime (`piPkg`/`directRunner`) leaks into it
  [system?](nix run .#test-stable-profile-membership)
- A downstream-pinned toolchain (`rustProfile { toolchain = ./rust-toolchain.toml }`) lands in tier 1 (`wrapix-stable-profile-<name>`), not the leaf
  [system?](nix run .#test-pinned-toolchain-stable-tier)
- A change to a tier-2 input (a downstream-appended package or the agent runtime `piPkg`/`directRunner`) leaves every tier-0 and tier-1 layer-blob byte-identical in the resulting image's manifest; only leaf-tier blobs change
  [system?](nix run .#test-downstream-change-leaf-only)
- `agent = "claude"` produces an image that contains `claude-code`
  [system](nix run .#test-claude-runtime-noop)
- The `agent = "pi"` code path threads a consumer-supplied `piPkg` derivation into the image build
  [check](grep -nE 'agent.*pi|piPkg' lib/sandbox/image.nix lib/sandbox/default.nix)
- `mkSandbox` throws a clear error when `agent = "pi"` is set without `piPkg`
  [check](grep -nE 'throw.*piPkg|piPkg.*requires' lib/sandbox/default.nix)
- The `agent = "direct"` code path threads a consumer-supplied `directRunner` derivation into the image build
  [check](grep -nE 'agent.*direct|directRunner' lib/sandbox/image.nix lib/sandbox/default.nix)
- `nix-command` and `flakes` are enabled in `/etc/nix/nix.conf` and Nix's in-container build sandbox is disabled
  [check](grep -nE 'experimental-features|sandbox' lib/sandbox/stable-profile-image.nix)
- CA certificates from `pkgs.cacert` are baked into the image and `SSL_CERT_FILE` resolves to the bundle
  [check](grep -nE 'cacert|SSL_CERT_FILE' lib/sandbox/image.nix)
- The platform entrypoint script (`lib/sandbox/{linux,darwin}/entrypoint.sh`) is the image's startup command
  [check](grep -nE 'entrypointSh|Entrypoint|Cmd' lib/sandbox/image.nix)
- `wrapix.prekHooks`, `wrapix.prePushChecks`, and `wrapix.skipIfMissing` all land in every profile image's store closure
  [check](grep -nrE 'prekHooks|prePushChecks|skipIfMissing' lib/sandbox/ lib/default.nix)
- The Linux entrypoint sets `core.hooksPath` on `/workspace/.git` to the `wrapix.prekHooks` store path when `.pre-commit-config.yaml` is present
  [check](grep -nE 'core\.hooksPath|prekHooks' lib/sandbox/linux/entrypoint.sh)
- The Darwin entrypoint mirrors the Linux entrypoint's `core.hooksPath` setup for `/workspace/.git`
  [check](grep -nE 'core\.hooksPath|prekHooks' lib/sandbox/darwin/entrypoint.sh)

## Requirements

### Functional

1. **OCI image generation** — `mkImage` returns a derivation whose output is an OCI-compatible image (streamLayeredImage on Linux, buildLayeredImage on Darwin).
2. **Package bundling** — every derivation in the profile's `packages` list lands in the image's store closure.
3. **Agent runtime composition** — the `agent` parameter selects which agent runtime layer the image carries: `claude` (default `claude-code` from nixpkgs), `pi` (consumer-supplied `piPkg`), or `direct` (consumer-supplied `directRunner`). The workspace profile composes orthogonally on top.
4. **Nix configuration** — `flakes` and `nix-command` are enabled; the in-container Nix sandbox is disabled (the outer container is the boundary).
5. **CA certificates** — `pkgs.cacert` is included and `SSL_CERT_FILE` resolves to it.
6. **Entrypoint embedding** — the platform-specific entrypoint script (`lib/sandbox/{linux,darwin}/entrypoint.sh`) is the image's startup command.
7. **Hook installation** — every profile image carries `wrapix.prekHooks` plus `wrapix.prePushChecks` and `wrapix.skipIfMissing` on `PATH`; the entrypoint configures `core.hooksPath` on `/workspace/.git` when `.pre-commit-config.yaml` is present. See `pre-commit.md` for the wrapper contracts.
8. **Provenance-tiered layering** — the image is a three-stage `fromImage` chain (base → stable-profile → leaf). Each tier's layer membership is fixed by its own closed contents and removes the union of all lower tiers' closures via `remove_paths`, so a tier-2 (downstream/volatile) change leaves tier-0 and tier-1 blobs byte-identical. Tier membership keys on `corePackages` (see `profiles.md`): the wrapix floor + profile toolchain + wrapix-generated content is tier 1; downstream-appended packages, the agent runtime, and per-invocation generated files are tier 2. Both Linux (`streamLayeredImage` leaf) and Darwin (`buildLayeredImage` leaf) chain identically; intermediate tiers are always `buildLayeredImage` tars so they can serve as a `fromImage` on both platforms.

### Non-Functional

1. **Layered for caching** — the provenance-tiered `fromImage` chain (see § Provenance-Tiered Layering) fixes each tier's layer membership by construction, so a change in one tier does not re-hash a lower tier's blobs.
2. **Reproducible** — same profile, agent selector, and consumer-supplied agent package (when applicable) produce the same image hash; `mkImageRef` is a pure function of the image.
3. **Iteration cost bounded by change size** — a change at tier 2 (a downstream-appended package, one MCP-config field, one entry in the Claude settings JSON, the agent runtime) re-emits only the leaf's delta; tier-0 and tier-1 blobs stay byte-identical, so the launcher's install step transfers O(change-size) bytes to the platform store, not O(image-size). On Linux this is achieved jointly by the provenance-tiered chain and the launcher's per-blob-dedup install transport (see `sandbox.md` § Image install path); on Darwin the delta is bounded by the tiered chain alone, with the per-blob-dedup install transport deferred (see *Out of Scope*).

## Out of Scope

- Multi-architecture manifests (arm64 + amd64 in a single ref)
- Image signing
- Registry push automation (consumers install from a Nix store path via the launcher's platform install path; remote registries are user-side concerns)
- Per-layer-blob-dedup install on Darwin — Apple's `container` CLI exposes only `container image load --input <tar>`, which walks the whole tar; neither a `skopeo` transport into Apple's container store nor an Apple-native per-blob-dedup install primitive has been verified. The Darwin launcher mitigates the gap with (1) the content-digest install-skip preflight (no-op when image already present) and (2) the provenance-tiered `fromImage` chain (the leaf tar shrinks to its tier-2 delta over the cached tier-0/tier-1 tars). Promoting Darwin to per-blob-dedup install requires a verified transport into the Apple `container` store.
