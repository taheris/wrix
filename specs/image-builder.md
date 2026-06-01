# OCI Image Builder

Nix expression that turns a wrapix profile into an OCI container image consumed by the platform launchers in `sandbox.md`.

## Problem Statement

Sandboxes need container images with all profile packages pre-installed, an agent runtime configured and ready, Nix configured for in-container package management, CA certificates for HTTPS, and the platform-specific entrypoint embedded. The builder produces these from a profile (`profiles.md`) plus an agent runtime axis (claude / pi / direct) without leaking platform-specific paths into the image expression.

## Architecture

`mkImage` (in `lib/sandbox/image.nix`) is the internal API called by `mkSandbox` (see `sandbox.md`). Inputs: a profile, an agent runtime selector, an entrypoint script path (Linux or Darwin), optional krun support, the merged Claude settings JSON, and the resolved MCP server configs. When `agent` is `"pi"` or `"direct"`, the caller also supplies the agent package (`piPkg` or `directRunner` respectively); `mkSandbox` throws when it's missing. It emits a layered OCI image via `dockerTools.streamLayeredImage` (Linux) or `dockerTools.buildLayeredImage` (Darwin — the stream script's Linux Python shebang cannot execute on macOS).

Image layout:

```
/
├── bin/, lib/, etc/     # Profile packages (from Nix store, layered)
├── entrypoint.sh        # Platform-specific startup script
└── etc/
    ├── nix/nix.conf     # Flakes + nix-command enabled, sandbox disabled
    └── ssl/certs/       # CA certificates for HTTPS
```

Build pipeline:

1. Collect packages from the profile (workspace toolchain + agent tooling + MCP server packages if any)
2. Compose the agent runtime layer (claude is a no-op; pi adds the consumer-supplied `piPkg` package; direct adds the consumer-supplied `directRunner` package)
3. Configure `/etc/nix/nix.conf` for flakes and disabled in-container sandbox (the outer container is the security boundary — see `specs/security.md`)
4. Bundle CA certificates from `pkgs.cacert`
5. Emit the layered image atop the shared `wrapix-base-image` via `fromImage`, with the entrypoint script as the OCI `Cmd`

`mkImageRef` (also in `lib/sandbox/`) produces the stable podman ref the launcher expects (`localhost/<name>:<hash-tag>` on Linux, bare `<name>:<hash-tag>` on Darwin's Apple `container` CLI).

## Base Image Layering

The image build is two-tier. A shared `wrapix-base-image` derivation pins the universal bottom-of-closure — toolchain libraries (glibc, gcc-lib, llvm), TLS material (openssl, cacert), shells and coreutils, and any other store paths that vary with the nixpkgs pin but not with profile inputs. Profile images chain on top via `fromImage`, so the base's tar is loaded into the platform store once and never re-emitted when profile-level inputs (`profile.packages`, `profile.env`, MCP configs, Claude settings, agent runtime selection) change.

Both `dockerTools.streamLayeredImage` (Linux) and `dockerTools.buildLayeredImage` (Darwin) accept `fromImage` with identical semantics, so the chaining shape is the same on both platforms.

Membership rule for the base: a store path belongs in `wrapix-base-image` iff it varies only with the nixpkgs pin, not with any profile-level input or wrapix-generated content. Paths whose hash depends on profile inputs — per-profile wrapper scripts, the merged Claude settings JSON, MCP configs, and the selected agent runtime layer (`piPkg`, `directRunner`, or `claude-code`) — stay in the per-profile top layers.

The top layers include `streamLayeredImage`'s **customisation layer** (its final layer, which aggregates content not held in a standalone Nix store path — generated files, metadata, anything the layered-image builder synthesises rather than pulling from the store). The customisation layer re-hashes whenever any of its inputs change; the membership rule keeps that input set tied to profile-level concerns so changes to a sibling profile don't perturb it.

## Hook Installation

Every profile image carries the host-equivalent prek setup so commits and pushes from inside the container fire the same `.pre-commit-config.yaml` chain the host runs (see `pre-commit.md` § Hook Installation in Profile Containers):

- `wrapix.prekHooks`, `wrapix.prePushChecks`, and `wrapix.skipIfMissing` are wired into the image's package set — `prekHooks` so its store path is reachable by `core.hooksPath`, and the two wrappers so they resolve on `PATH` for prek `entry:` lines that reference them by name.
- The platform entrypoint (`lib/sandbox/{linux,darwin}/entrypoint.sh`) sets `core.hooksPath` on `/workspace/.git` to the `wrapix.prekHooks` store path when `.pre-commit-config.yaml` is present, mirroring `mkDevShell`'s host-side step.
- Profile images for bead use do not ship `nix` by default; nix-requiring hooks remain inert under `skip-if-missing nix --` (see `pre-commit.md`). The image builder does not inject `SKIP=` env vars, does not stub `nix` on `PATH`, and does not maintain a hook-id skip list. A profile that wants nix to fire in-container ships `pkgs.nix` via its own `packages`.

## Success Criteria

- The launcher's image-install step (under both `wrapix run` and `wrapix spawn`) is short-circuited when the image's content digest is already present in the platform store: no tar materialization, no stream invocation, no `*-load` CLI call
  [system](nix run .#test-image-install-digest-skip)
- `mkImage` chains the profile image atop the shared `wrapix-base-image` derivation via `fromImage` on both Linux (`streamLayeredImage`) and Darwin (`buildLayeredImage`)
  [check?](grep -nE 'fromImage|wrapix-base-image' lib/sandbox/image.nix)
- `wrapix-base-image`'s derivation hash is invariant under changes to profile-level inputs — `profile.packages`, `profile.env`, MCP configs, the merged Claude settings JSON, and the agent runtime selection
  [system?](nix run .#test-base-image-hash-stable)
- A one-file perturbation in profile-level inputs (one wrapper script touched) leaves every layer-blob hash in the resulting image's manifest unchanged except for the customisation layer and any top layer that directly depends on the changed file
  [system?](nix run .#test-iteration-cost-bounded)
- `agent = "claude"` produces an image that contains `claude-code`
  [system](nix run .#test-claude-runtime-noop)
- The `agent = "pi"` code path threads a consumer-supplied `piPkg` derivation into the image build
  [check](grep -nE 'agent.*pi|piPkg' lib/sandbox/image.nix lib/sandbox/default.nix)
- `mkSandbox` throws a clear error when `agent = "pi"` is set without `piPkg`
  [check](grep -nE 'throw.*piPkg|piPkg.*requires' lib/sandbox/default.nix)
- The `agent = "direct"` code path threads a consumer-supplied `directRunner` derivation into the image build
  [check](grep -nE 'agent.*direct|directRunner' lib/sandbox/image.nix lib/sandbox/default.nix)
- `nix-command` and `flakes` are enabled in `/etc/nix/nix.conf` and Nix's in-container build sandbox is disabled
  [check](grep -nE 'experimental-features|sandbox' lib/sandbox/image.nix)
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
3. **Agent runtime composition** — the `agent` parameter selects which agent runtime layer the image carries: `claude` (from nixpkgs), `pi` (consumer-supplied `piPkg`), or `direct` (consumer-supplied `directRunner`). The workspace profile composes orthogonally on top.
4. **Nix configuration** — `flakes` and `nix-command` are enabled; the in-container Nix sandbox is disabled (the outer container is the boundary).
5. **CA certificates** — `pkgs.cacert` is included and `SSL_CERT_FILE` resolves to it.
6. **Entrypoint embedding** — the platform-specific entrypoint script (`lib/sandbox/{linux,darwin}/entrypoint.sh`) is the image's startup command.
7. **Hook installation** — every profile image carries `wrapix.prekHooks` plus `wrapix.prePushChecks` and `wrapix.skipIfMissing` on `PATH`; the entrypoint configures `core.hooksPath` on `/workspace/.git` when `.pre-commit-config.yaml` is present. See `pre-commit.md` for the wrapper contracts.
8. **Base image layering** — `mkImage` chains the profile image atop a shared `wrapix-base-image` derivation via `fromImage`. The base captures the universal bottom-of-closure (nixpkgs-pin-dependent only); per-profile layers ride above. Both Linux (`streamLayeredImage`) and Darwin (`buildLayeredImage`) image builders use the same chaining.

### Non-Functional

1. **Layered for caching** — packages, agent runtime, and config land in separate layers so changes in one do not invalidate the others.
2. **Reproducible** — same profile, agent selector, and consumer-supplied agent package (when applicable) produce the same image hash; `mkImageRef` is a pure function of the image.
3. **Iteration cost bounded by change size** — a small change to profile-level inputs (one wrapper script, one MCP-config field, one entry in the Claude settings JSON) produces a small delta on top of `wrapix-base-image`. The launcher's install step on the resulting image transfers O(change-size) bytes to the platform store, not O(image-size). On Linux this is achieved jointly by base-image pinning and the launcher's per-blob-dedup install transport (see `sandbox.md` § Image install path); on Darwin the delta is bounded by base-image pinning alone, with the per-blob-dedup install transport deferred (see *Out of Scope*).

## Out of Scope

- Multi-architecture manifests (arm64 + amd64 in a single ref)
- Image signing
- Registry push automation (consumers install from a Nix store path via the launcher's platform install path; remote registries are user-side concerns)
- Per-layer-blob-dedup install on Darwin — Apple's `container` CLI exposes only `container image load --input <tar>`, which walks the whole tar; neither a `skopeo` transport into Apple's container store nor an Apple-native per-blob-dedup install primitive has been verified. The Darwin launcher mitigates the gap with (1) the content-digest install-skip preflight (no-op when image already present) and (2) `wrapix-base-image` chaining (per-profile tar shrinks to top-of-closure delta). Promoting Darwin to per-blob-dedup install requires a verified transport into the Apple `container` store.
