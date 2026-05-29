# OCI Image Builder

Nix expression that turns a wrapix profile into an OCI container image consumed by the platform launchers in `sandbox.md`.

## Problem Statement

Sandboxes need container images with all profile packages pre-installed, an agent runtime configured and ready, Nix configured for in-container package management, CA certificates for HTTPS, and the platform-specific entrypoint embedded. The builder produces these from a profile (`profiles.md`) plus an agent runtime axis (claude / pi / direct) without leaking platform-specific paths into the image expression.

## Architecture

`mkImage` (in `lib/sandbox/image.nix`) is the internal API called by `mkSandbox` (see `sandbox.md`). Inputs: a profile, an agent runtime selector, an entrypoint script path (Linux or Darwin), optional krun support, the merged Claude settings JSON, and the resolved MCP server configs. It emits a layered OCI image via `dockerTools.streamLayeredImage` (Linux) or `dockerTools.buildLayeredImage` (Darwin — the stream script's Linux Python shebang cannot execute on macOS).

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
2. Compose the agent runtime layer (claude is a no-op; pi adds Node + pi-mono; direct adds the consumer-supplied `directRunner` package)
3. Configure `/etc/nix/nix.conf` for flakes and disabled in-container sandbox (the outer container is the security boundary — see `specs/security.md`)
4. Bundle CA certificates from `pkgs.cacert`
5. Emit the layered image, with the entrypoint script as the OCI `Cmd`

`mkImageRef` (also in `lib/sandbox/`) produces the stable podman ref the launcher expects (`localhost/<name>:<hash-tag>` on Linux, bare `<name>:<hash-tag>` on Darwin's Apple `container` CLI).

## Hook Installation

Every profile image carries the host-equivalent prek setup so commits and pushes from inside the container fire the same `.pre-commit-config.yaml` chain the host runs (see `pre-commit.md` § Bead-Container Hook Installation):

- `wrapix.prekHooks`, `wrapix.prePushChecks`, and `wrapix.skipIfMissing` are wired into the image's package set — `prekHooks` so its store path is reachable by `core.hooksPath`, and the two wrappers so they resolve on `PATH` for prek `entry:` lines that reference them by name.
- The platform entrypoint (`lib/sandbox/{linux,darwin}/entrypoint.sh`) sets `core.hooksPath` on `/workspace/.git` to the `wrapix.prekHooks` store path when `.pre-commit-config.yaml` is present, mirroring `mkDevShell`'s host-side step.
- Profile images for bead use do not ship `nix` by default; nix-requiring hooks remain inert under `skip-if-missing nix --` (see `pre-commit.md`). The image builder does not inject `SKIP=` env vars, does not stub `nix` on `PATH`, and does not maintain a hook-id skip list. A profile that wants nix to fire in-container ships `pkgs.nix` via its own `packages`.

## Success Criteria

- `wrapix spawn`'s image-source → `podman load` step is idempotent (re-loading the same image is a no-op on a fresh inode and on a previously-loaded ref)
  [system](nix run .#wrapix-spawn-load)
- `agent = "pi"` adds an executable `pi` binary to the image's store closure
  [system](nix run .#pi-runtime-image)
- `agent = "claude"` produces an image that contains `claude-code` but does not pull in `pi-mono`
  [system](nix run .#claude-runtime-noop)
- The `agent = "direct"` code path threads a consumer-supplied `directRunner` derivation into the image build
  [check](grep -nE 'agent.*direct|directRunner' lib/sandbox/image.nix lib/sandbox/default.nix)
- `nix-command` and `flakes` are enabled in `/etc/nix/nix.conf` and Nix's in-container build sandbox is disabled
  [check](grep -nE 'experimental-features|sandbox' lib/sandbox/image.nix)
- CA certificates from `pkgs.cacert` are baked into the image and `SSL_CERT_FILE` resolves to the bundle
  [check](grep -nE 'cacert|SSL_CERT_FILE' lib/sandbox/image.nix)
- The platform entrypoint script (`lib/sandbox/{linux,darwin}/entrypoint.sh`) is the image's startup command
  [check](grep -nE 'entrypointSh|Entrypoint|Cmd' lib/sandbox/image.nix)
- `wrapix.prekHooks`, `wrapix.prePushChecks`, and `wrapix.skipIfMissing` all land in every profile image's store closure
  [check?](grep -nrE 'prekHooks|prePushChecks|skipIfMissing' lib/sandbox/ lib/default.nix)
- The Linux entrypoint sets `core.hooksPath` on `/workspace/.git` to the `wrapix.prekHooks` store path when `.pre-commit-config.yaml` is present
  [check?](grep -nE 'core\.hooksPath|prekHooks' lib/sandbox/linux/entrypoint.sh)
- The Darwin entrypoint mirrors the Linux entrypoint's `core.hooksPath` setup for `/workspace/.git`
  [check?](grep -nE 'core\.hooksPath|prekHooks' lib/sandbox/darwin/entrypoint.sh)

## Requirements

### Functional

1. **OCI image generation** — `mkImage` returns a derivation whose output is an OCI-compatible image (streamLayeredImage on Linux, buildLayeredImage on Darwin).
2. **Package bundling** — every derivation in the profile's `packages` list lands in the image's store closure.
3. **Agent runtime composition** — the `agent` parameter selects which agent runtime layer the image carries; the workspace profile composes orthogonally on top.
4. **Nix configuration** — `flakes` and `nix-command` are enabled; the in-container Nix sandbox is disabled (the outer container is the boundary).
5. **CA certificates** — `pkgs.cacert` is included and `SSL_CERT_FILE` resolves to it.
6. **Entrypoint embedding** — the platform-specific entrypoint script (`lib/sandbox/{linux,darwin}/entrypoint.sh`) is the image's startup command.
7. **Hook installation** — every profile image carries `wrapix.prekHooks` plus `wrapix.prePushChecks` and `wrapix.skipIfMissing` on `PATH`; the entrypoint configures `core.hooksPath` on `/workspace/.git` when `.pre-commit-config.yaml` is present. See `pre-commit.md` for the wrapper contracts.

### Non-Functional

1. **Layered for caching** — packages, agent runtime, and config land in separate layers so changes in one do not invalidate the others.
2. **Reproducible** — same profile + agent selector produces the same image hash; `mkImageRef` is a pure function of the image.

## Out of Scope

- Multi-architecture manifests (arm64 + amd64 in a single ref)
- Image signing
- Registry push automation (consumers `podman load` from a Nix store path; remote registries are user-side concerns)
