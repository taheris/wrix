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

## Requirements

### Functional

1. **OCI image generation** — `mkImage` returns a derivation whose output is an OCI-compatible image (streamLayeredImage on Linux, buildLayeredImage on Darwin).
2. **Package bundling** — every derivation in the profile's `packages` list lands in the image's store closure.
3. **Agent runtime composition** — the `agent` parameter selects which agent runtime layer the image carries; the workspace profile composes orthogonally on top.
4. **Nix configuration** — `flakes` and `nix-command` are enabled; the in-container Nix sandbox is disabled (the outer container is the boundary).
5. **CA certificates** — `pkgs.cacert` is included and `SSL_CERT_FILE` resolves to it.
6. **Entrypoint embedding** — the platform-specific entrypoint script (`lib/sandbox/{linux,darwin}/entrypoint.sh`) is the image's startup command.

### Non-Functional

1. **Layered for caching** — packages, agent runtime, and config land in separate layers so changes in one do not invalidate the others.
2. **Reproducible** — same profile + agent selector produces the same image hash; `mkImageRef` is a pure function of the image.

## Out of Scope

- Multi-architecture manifests (arm64 + amd64 in a single ref)
- Image signing
- Registry push automation (consumers `podman load` from a Nix store path; remote registries are user-side concerns)
