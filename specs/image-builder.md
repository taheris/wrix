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
2. Compose the agent tier (`wrapix-agent-<agent>-<name>`) with exactly the selected agent runtime — `claude-code` (claude), the consumer-supplied `piPkg` (pi), or `directRunner` (direct); a non-selected agent's binary is not present
3. Configure `/etc/nix/nix.conf` for flakes and disabled in-container sandbox (the outer container is the security boundary — see `specs/security.md`)
4. Bundle CA certificates from `pkgs.cacert`
5. Emit the leaf image atop the `wrapix-agent-<agent>-<name>` → `wrapix-stable-profile-<name>` → `wrapix-base-image` chain via `fromImage` (see § Provenance-Tiered Layering), with the entrypoint script as the OCI `Cmd`

`mkImageRef` (also in `lib/sandbox/`) produces the stable podman ref the launcher expects (`localhost/<name>:<hash-tag>` on Linux, bare `<name>:<hash-tag>` on Darwin's Apple `container` CLI).

## Provenance-Tiered Layering

The image is a **four-stage `fromImage` chain**, each stage a derivation whose layer membership is fixed by its own closed set of contents. Chaining is the only mechanism that makes a store path's layer assignment independent of the rest of the image — the property skopeo's per-blob install transport (`sandbox.md` § Image install path) needs to dedup across rebuilds. A single image's automatic layering derives membership from the *global* path-popularity ranking, so adding or removing one input re-partitions which paths share a layer and re-hashes every affected blob (the install transport then re-transfers them instead of reusing the cache). Fixing each tier's contents eliminates that re-partition: a path keeps its layer regardless of what changes in the tiers above or below it.

The four tiers, bottom (most stable) to top (most volatile):

| Tier | Derivation | Contents | Changes with |
|------|-----------|----------|--------------|
| 0 — base | `wrapix-base-image` | universal nixpkgs bottom-of-closure (glibc, gcc-lib, openssl, cacert, shells, coreutils) | nixpkgs pin |
| 1 — stable-profile | `wrapix-stable-profile-<name>` | the profile's `corePackages` (the `basePackages` floor + the profile toolchain, default *or* pinned) plus wrapix-generated derivations (notify client, prek wrappers + bundle, nix.conf, passwd/group) — **no agent runtime** | wrapix source, nixpkgs pin, a toolchain re-pin |
| 2 — agent | `wrapix-agent-<agent>-<name>` | exactly the **one** selected agent runtime and its closure — `claude-code` (claude), the consumer-supplied `piPkg` (pi), or `directRunner` (direct). A non-selected agent's binary is absent | the agent selection and that agent package's own version |
| 3 — leaf | the per-profile image | downstream-appended packages (`profile.packages` − `corePackages`) and per-invocation generated content (merged Claude settings + `model`, MCP configs, the `profileEnv` symlink tree, entrypoint `extraCommands`), plus the Nix DB registration file | each consumer iteration |

Tiers 1 and 2 are built with `dockerTools.buildLayeredImage` (tars in the store) so each can serve as a `fromImage` on both Linux and Darwin — the same tar-only constraint `base-image.nix` records for tier 0. The leaf is `streamLayeredImage` on Linux and `buildLayeredImage` on Darwin. Each non-base tier sets `fromImage` to the tier below — leaf → agent → stable-profile → base — and drives its `layeringPipeline` with `remove_paths` over the **union of all lower tiers' closures**, so no tier re-emits a path a lower tier already ships. Within a tier, standard dockerTools layering applies, but because the tier's contents are a fixed set the ordering inside it is stable — the global-ranking re-partition that destabilised the old single-image layering cannot occur.

**Tier membership rule.** Three discriminators separate the three non-base tiers. *Fixed-per-profile-instance* content (tier 1) is the profile's `corePackages` plus the wrapix-generated derivations; the profile exposes it through `corePackages` (see `profiles.md` § Profile Attrset Schema), the wrapix-controlled set `mkProfile` fixes at construction — downstream extension (`deriveProfile`, `rustProfile { packages }`) appends only to `profile.packages`, never to `corePackages`. The *one selected agent runtime* (`claude-code`/`piPkg`/`directRunner`) is tier 2. *Varies-per-iteration* content — the appended delta `profile.packages` − `corePackages` and the volatile generated files — is tier 3 (leaf). A downstream-**pinned** toolchain (`rustProfile { toolchain = ./rust-toolchain.toml }`) is part of `corePackages` and therefore tier 1.

**Tier ordering (toolchain below agent).** The agent tier sits *above* the toolchain, not below, because a change low in the chain re-emits every tier above it, and the toolchain is by far the heaviest mutable path (a pinned rust toolchain is ~1.5 GB versus ~230 MB for `claude-code` and less for a `direct` runner). Placing the toolchain lowest — only `wrapix-base-image` beneath it — means almost nothing ever drags it: it re-ships only when it itself changes. The inverse order would re-ship the whole toolchain on every agent-version bump, and for `direct` — whose runner is the consumer's own frequently-rebuilt binary — that would be on nearly every iteration. The light agent binary is the cheap thing to cascade, so it rides above; cascade cost is `freq × weight-of-tier-dragged`, and the toolchain's weight dominates regardless of relative cadence.

**Tier 0 membership.** A path belongs in `wrapix-base-image` iff (a) it varies only with the nixpkgs pin, not with any profile-level input or wrapix-generated content, **and** (b) it is genuinely universal — every profile already closes over it as shared bottom-of-closure, not a profile-specific leaf. Condition (a) alone is necessary but not sufficient. A pin-stable but non-universal path must not be hoisted into tier 0 merely because its hash tracks only the pin: doing so would force every profile image to carry it even where it is never closed over. A profile-specific compiler toolchain such as `pkgs.rustc` is the case in point — base and python close over no Rust at all, and even the rust profile pulls fenix's toolchain (a different store path), so `pkgs.rustc` in tier 0 would be dead weight in every image. Such a path stays in the profile-specific tier 1 domain, never tier 0.

The leaf's **customisation layer** (its final layer — generated files, metadata, and anything not held in a standalone Nix store path, including the `profileEnv` symlink tree) re-hashes whenever any of its inputs change, but it rides above the stable tiers, so a tier-3-only (leaf) change leaves every tier-0, tier-1, and tier-2 blob byte-identical. The `profileEnv` symlink tree references tier-0, tier-1, and tier-2 store paths that resolve from the lower layers at runtime, so the leaf's `remove_paths` strips them from its own graph without breaking PATH. The 127-layer OCI ceiling now splits four ways: tiers 0, 1, and 2 bound `maxLayers` to their fixed closures, and the leaf budgets only its delta plus the customisation layer.

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
intermediates (the `wrapix-*-profile-env` buildEnv and image-build artifacts
such as the customisation-layer tar and `layering.json`) that are never copied
into the rootfs; registering them is precisely what bakes a dangling path.
Computing the registration (`closureInfo` over the materialized path list +
`nix-store --load-db`) from that same list keeps the DB an honest description
of the disk.

This is also the cache-optimal choice: registering exactly what is already
materialized copies up **no** new store path, so every tier-0, tier-1, and
tier-2 blob stays byte-identical across rebuilds and only the single DB file
in the leaf's customisation layer re-hashes, at that layer's existing cadence —
the provenance-tiered chain is unaffected. Materializing the missing
intermediates into the rootfs instead would add store-path copy-up and re-hash
a lower tier, trading the consistency bug for a caching regression.

The runtime acceptance — a fresh container letting the runtime user run
`nix develop` / `nix build` and mutate baked store paths — is owned by
`sandbox.md`.

## Hook Installation

Every profile image carries the host-equivalent prek setup so commits and pushes from inside the container fire the same `.pre-commit-config.yaml` chain the host runs (see `pre-commit.md` § Hook Installation in Profile Containers):

- `wrapix.prekHooks`, `wrapix.prePushChecks`, and `wrapix.skipIfMissing` are wired into the image's package set — `prekHooks` so its store path is reachable by `core.hooksPath`, and the two wrappers so they resolve on `PATH` for prek `entry:` lines that reference them by name.
- The platform entrypoint (`lib/sandbox/{linux,darwin}/entrypoint.sh`) sets `core.hooksPath` on `/workspace/.git` to the `wrapix.prekHooks` store path when `.pre-commit-config.yaml` is present, mirroring `mkDevShell`'s host-side step.
- Profile images for bead use do not ship `nix` by default; nix-requiring hooks remain inert under `skip-if-missing nix --` (see `pre-commit.md`). The image builder does not inject `SKIP=` env vars, does not stub `nix` on `PATH`, and does not maintain a hook-id skip list. A profile that wants nix to fire in-container ships `pkgs.nix` via its own `packages`.

## Success Criteria

- The launcher's image-install step (under both `wrapix run` and `wrapix spawn`) is short-circuited when the image's content digest is already present in the platform store: no tar materialization, no stream invocation, no `*-load` CLI call
  [system](nix run .#test-image-install-digest-skip)
- The image is a four-stage `fromImage` chain — the leaf chains atop `wrapix-agent-<agent>-<name>`, which chains atop `wrapix-stable-profile-<name>`, which chains atop `wrapix-base-image` — on both Linux (`streamLayeredImage` leaf) and Darwin (`buildLayeredImage` leaf)
  [check](grep -nE 'wrapix-agent' lib/sandbox/image.nix)
- Each non-base tier removes the union of all lower tiers' closures from its layering graph via `remove_paths`, so no tier re-emits a store path a lower tier already ships
  [check](grep -nE 'remove_paths' lib/sandbox/image.nix lib/sandbox/stable-profile-image.nix)
- `wrapix-base-image`'s derivation hash is invariant under changes to profile-level inputs — `profile.packages`, `profile.env`, MCP configs, the merged Claude settings JSON, and the agent runtime selection
  [system](nix run .#test-base-image-hash-stable)
- `wrapix-base-image` holds only the universal bottom-of-closure: no profile-specific compiler toolchain leaks in (e.g. `pkgs.rustc`, which no profile references — the rust profile uses fenix's toolchain)
  [system](nix run .#test-base-image-universal)
- `wrapix-stable-profile-<name>`'s derivation hash is invariant under the agent runtime selection and all leaf (tier-3) inputs — downstream-appended packages (`profile.packages` − `corePackages`), the merged Claude settings JSON, MCP configs, and `model`
  [system](nix run .#test-stable-profile-hash-stable)
- `wrapix-stable-profile-<name>` holds only fixed-per-instance content: no agent runtime — not the default `claude-code`, nor a consumer-supplied `piPkg`/`directRunner` — and no downstream-appended package leaks into it
  [system](nix run .#test-stable-profile-membership)
- A downstream-pinned toolchain (`rustProfile { toolchain = ./rust-toolchain.toml }`) lands in tier 1 (`wrapix-stable-profile-<name>`), not the leaf
  [system](nix run .#test-pinned-toolchain-stable-tier)
- A tier-3 (leaf) change — a downstream-appended package, a Claude-settings field, an MCP-config field, or `model` — leaves every tier-0, tier-1, and tier-2 (agent) layer-blob byte-identical in the resulting image's manifest; only leaf blobs change
  [system](nix run .#test-downstream-change-leaf-only)
- The selected agent runtime rides its own tier `wrapix-agent-<agent>-<name>`, chained atop `wrapix-stable-profile-<name>`; an agent-version change leaves every tier-0 and tier-1 blob byte-identical
  [system](nix run .#test-agent-tier-isolated)
- A non-selected agent's binary is absent from the image: an `agent = "direct"` image contains neither `claude-code` nor a `pi` runtime
  [system](nix run .#test-agent-exclusive)
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
- The baked image's Nix database registers no orphaned path: every path on disk under `/nix/store` is registered valid
  [system](nix run .#test-image-nix-db-consistent)
- The baked image's Nix database registers no dangling path: every path registered valid exists on disk, so a freshly provisioned container passes `nix-store --verify --check-contents` with zero missing paths and an additive `nix build` cannot fail with `No such file or directory` on a registered path. The registration is derived from the materialized contents closure, not the full build closure
  [system](nix run .#test-image-nix-db-no-dangling)
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
3. **Agent runtime composition** — the `agent` parameter selects the single agent runtime the image carries: `claude` (default `claude-code` from nixpkgs), `pi` (consumer-supplied `piPkg`), or `direct` (consumer-supplied `directRunner`). Exactly one agent is baked — a non-selected agent's binary is absent (a `direct` image carries no `claude-code`) — and it rides its own tier (tier 2, see § Provenance-Tiered Layering), composing orthogonally with the workspace profile.
4. **Nix configuration** — `flakes` and `nix-command` are enabled; the in-container Nix sandbox is disabled (the outer container is the boundary).
5. **CA certificates** — `pkgs.cacert` is included and `SSL_CERT_FILE` resolves to it.
6. **Entrypoint embedding** — the platform-specific entrypoint script (`lib/sandbox/{linux,darwin}/entrypoint.sh`) is the image's startup command.
7. **Hook installation** — every profile image carries `wrapix.prekHooks` plus `wrapix.prePushChecks` and `wrapix.skipIfMissing` on `PATH`; the entrypoint configures `core.hooksPath` on `/workspace/.git` when `.pre-commit-config.yaml` is present. See `pre-commit.md` for the wrapper contracts.
8. **Provenance-tiered layering** — the image is a four-stage `fromImage` chain (base → stable-profile → agent → leaf). Each tier's layer membership is fixed by its own closed contents and removes the union of all lower tiers' closures via `remove_paths`, so a tier-3 (leaf) change leaves tier-0, tier-1, and tier-2 blobs byte-identical. Tier membership: the wrapix floor + profile toolchain + wrapix-generated content is tier 1 (keyed on `corePackages`, see `profiles.md`); the single selected agent runtime (`claude-code`/`piPkg`/`directRunner`) is tier 2; downstream-appended packages and per-invocation generated files are tier 3. The agent tier rides *above* the toolchain so an agent-version bump never re-ships the heavier toolchain (which sits lowest, dragged only by a base change). Both Linux (`streamLayeredImage` leaf) and Darwin (`buildLayeredImage` leaf) chain identically; intermediate tiers (stable-profile, agent) are always `buildLayeredImage` tars so they can serve as a `fromImage` on both platforms.
9. **Store/DB consistency** — the image's Nix database registers *exactly* its on-disk contents closure: no orphaned (on-disk but unregistered) path and no dangling (registered but absent) path, in either direction. The registered set is derived from the materialized path list copied into the image layers, not the build derivation's full closure (which includes unmaterialized intermediates like the `wrapix-*-profile-env` buildEnv). This prevents a build from trusting a registered-but-missing path (`No such file or directory`, unrecoverable without store surgery); the no-orphan direction is no longer a correctness requirement now that the runtime user owns the store, but registering over exactly the materialized contents secures it for free. The registration rides as a single DB file in the leaf customisation layer — it copies up no store path and does not perturb the provenance-tiered chain. Runtime acceptance is owned by `sandbox.md`.

### Non-Functional

1. **Layered for caching** — the provenance-tiered `fromImage` chain (see § Provenance-Tiered Layering) fixes each tier's layer membership by construction, so a change in one tier does not re-hash a lower tier's blobs.
2. **Reproducible** — same profile, agent selector, and consumer-supplied agent package (when applicable) produce the same image hash; `mkImageRef` is a pure function of the image.
3. **Iteration cost bounded by change size** — a change at tier 3 (a downstream-appended package, one MCP-config field, one entry in the Claude settings JSON, `model`) re-emits only the leaf's delta; tier-0, tier-1, and tier-2 (agent) blobs stay byte-identical, so the launcher's install step transfers O(change-size) bytes to the platform store, not O(image-size). An agent-version bump re-emits only the agent tier and the leaf, leaving the heavier tier-0/tier-1 blobs byte-identical. On Linux this is achieved jointly by the provenance-tiered chain and the launcher's per-blob-dedup install transport (see `sandbox.md` § Image install path); on Darwin the delta is bounded by the tiered chain alone, with the per-blob-dedup install transport deferred (see *Out of Scope*).

## Out of Scope

- Multi-architecture manifests (arm64 + amd64 in a single ref)
- Image signing
- Registry push automation (consumers install from a Nix store path via the launcher's platform install path; remote registries are user-side concerns)
- Per-layer-blob-dedup install on Darwin — Apple's `container` CLI exposes only `container image load --input <tar>`, which walks the whole tar; neither a `skopeo` transport into Apple's container store nor an Apple-native per-blob-dedup install primitive has been verified. The Darwin launcher mitigates the gap with (1) the content-digest install-skip preflight (no-op when image already present) and (2) the provenance-tiered `fromImage` chain (the leaf tar shrinks to its tier-3 leaf delta over the cached tier-0/tier-1/tier-2 tars). Promoting Darwin to per-blob-dedup install requires a verified transport into the Apple `container` store.
