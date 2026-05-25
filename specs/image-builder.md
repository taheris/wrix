# OCI Image Builder

Nix-based container image creation for sandbox environments.

## Problem Statement

Sandboxes need container images with:
- All profile packages pre-installed
- Claude Code configured and ready
- Proper Nix configuration for in-container package management
- Entrypoint scripts for runtime setup

## Requirements

### Functional

1. **OCI Image Generation** - Produce OCI-compliant container images
2. **Package Bundling** - Include all profile packages in image layers
3. **Claude Code Integration** - Embed Claude Code with proper configuration
4. **Nix Configuration** - Enable flakes and nix-command features
5. **CA Certificates** - Include root certificates for HTTPS
6. **Entrypoint Embedding** - Include platform-specific startup scripts

### Non-Functional

1. **Layered Images** - Efficient layer structure for caching
2. **Minimal Size** - Only include necessary packages
3. **Reproducible** - Same inputs produce identical images

## Image Contents

```
/
├── bin/, lib/, etc/     # Profile packages (from Nix store)
├── entrypoint.sh        # Container startup script
└── etc/
    ├── nix/nix.conf     # Nix configuration
    └── ssl/certs/       # CA certificates
```

## Build Process

1. Collect packages from profile
2. Add Claude Code package
3. Configure Nix settings (flakes, sandbox)
4. Bundle CA certificates
5. Create OCI image with dockerTools

## Affected Files

| File | Role |
|------|------|
| `lib/sandbox/image.nix` | Image builder function |
| `lib/sandbox/default.nix` | Calls image builder with profile |
| `lib/sandbox/*/entrypoint.sh` | Platform-specific startup |

## API

```nix
# Internal API (called by mkSandbox)
mkImage {
  profile = profiles.rust;
  claudeCode = pkgs.claude-code;
  entrypoint = ./linux/entrypoint.sh;
}
```

## Success Criteria

- [ ] Generated images are valid OCI format
  [judge](../tests/judges/image-builder.sh#test_oci_format)
- [ ] All profile packages are available in container
  [judge](../tests/judges/image-builder.sh#test_profile_packages)
- [ ] Claude Code starts correctly
  [judge](../tests/judges/image-builder.sh#test_claude_code_starts)
- [ ] Nix commands work inside container
  [judge](../tests/judges/image-builder.sh#test_nix_in_container)
- [ ] HTTPS connections succeed (CA certs present)
  [verify:wrapix](../tests/darwin/network-test.sh)

## Out of Scope

- Multi-architecture images (arm64/amd64 in same manifest)
- Image signing
- Registry push automation
