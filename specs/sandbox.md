# Core Sandbox

Secure container isolation for running Claude Code with filesystem and process protection.

## Problem Statement

Running AI coding assistants with unrestricted host access creates security risks. Users need isolation that:
- Protects host filesystem and processes from container actions
- Maintains correct file ownership for workspace files
- Provides full network access for research and package management
- Works consistently across Linux and macOS

## Requirements

### Functional

1. **Container Creation** - `mkSandbox` function creates runnable sandbox derivations. Returns `{ package, image, profile }` where `image` is the per-profile OCI artifact and `package` is a `makeWrapper` composing the profile-agnostic launcher (`packages.wrapix`) with that image. Consumers driving podman themselves read `.image` directly; one-shot users invoke `.package`
2. **Platform Dispatch** - Automatically selects Podman (Linux) or Apple container CLI (macOS)
3. **Workspace Mounting** - Current directory mounted at `/workspace` with read-write access
4. **User Namespace Mapping** - Files created in container have correct host UID/GID
5. **Custom Mounts** - Support additional read-only or read-write mounts
6. **Environment Variables** - Pass custom environment variables to container
7. **Deploy Keys** - SSH key injection for git push operations

### Non-Functional

1. **No Elevated Privileges** - Containers run without root or elevated capabilities
2. **Full Network Access** - Unrestricted TCP/UDP for web research (ICMP unavailable without `cap_net_raw`)
3. **Near-Native Performance** - Minimal overhead from containerization

## Platform Implementations

### Linux (Podman)

- Uses `--network=pasta` for userspace networking (open outbound, no inbound ports)
- Uses `--userns=keep-id` for UID mapping
- Mounts workspace via bind mount

### macOS (Apple Container CLI)

- Requires macOS 26+ and Apple Silicon
- Uses Virtualization.framework for lightweight VMs
- Uses vmnet for networking (open outbound, no inbound ports)
- Uses virtio-fs for workspace mounting
- Entrypoint creates user matching host UID

## Launcher Subcommands

The `wrapix` launcher binary (`packages.wrapix`, profile-agnostic) exposes
two subcommands. Both share container construction (mounts, env passthrough,
runtime selection, deploy key, beads socket); they differ only in stdio and
where the spawn parameters come from.

| Subcommand | Stdio | Configuration source | Use case |
|------------|-------|----------------------|----------|
| `wrapix run [DIR] [CMD…]` | TTY (`-it`) | Host environment + CLI args | Interactive sessions, `nix run .#sandbox-<profile>` |
| `wrapix spawn --spawn-config <file> [--stdio]` | Piped (`--stdio`) or detached | JSON file (typed `SpawnConfig`) | Programmatic dispatch (loom; future orchestrators) |

The `SpawnConfig` JSON has stable top-level fields: `image_ref` (podman ref),
`image_source` (Nix store path the launcher loads via `podman load` before
invoking podman; idempotent on the image's hash tag), `workspace`, `env`
(allowlist of `[key, value]` pairs), `agent_args`, plus consumer-defined
fields the entrypoint reads from inside the container. External orchestrators
(e.g. Loom) are the only producers in practice; the schema is part of the
wrapix CLI contract — see `wrapix spawn --help` and the parsing block in
`lib/sandbox/{linux,darwin}/default.nix`.

`wrapix run` (interactive) has no `--spawn-config` so it reads two env
vars to know which image to load: `WRAPIX_DEFAULT_IMAGE_REF` (podman ref)
and `WRAPIX_DEFAULT_IMAGE_SOURCE` (Nix store path). The convenience flake
outputs `packages.sandbox-<profile>` set both via `makeWrapper`; loom plan
sets them programmatically from the profile-image manifest before exec.
Without these vars set, `wrapix run` errors at startup — there is no
implicit default image baked into the launcher.

## Affected Files

| File | Role |
|------|------|
| `lib/sandbox/default.nix` | Platform dispatcher and mkSandbox API |
| `lib/sandbox/linux/default.nix` | Podman launcher script |
| `lib/sandbox/darwin/default.nix` | Apple container launcher script |
| `lib/sandbox/linux/entrypoint.sh` | Linux container startup |
| `lib/sandbox/darwin/entrypoint.sh` | macOS container startup |

## API

```nix
mkSandbox {
  profile = profiles.base;      # Development profile
  deployKey = "myproject";      # SSH key name for git push
  packages = [ pkgs.jq ];       # Additional packages
  env = { FOO = "bar"; };       # Environment variables
  mounts = [{                   # Additional mounts
    source = "~/.config";
    dest = "~/.config";
    mode = "ro";
  }];
}
```

## Success Criteria

- [ ] Container starts on both Linux and macOS
  [verify:wrapix](../tests/darwin/uid-test.sh)
- [ ] Files created in /workspace have correct host ownership
  [verify:wrapix](../tests/darwin/uid-test.sh)
- [ ] Claude Code can access internet for research
  [verify:wrapix](../tests/darwin/network-test.sh)
- [ ] Host filesystem outside /workspace is inaccessible
  [verify:wrapix](../tests/darwin/mount-test.sh)
- [ ] Custom mounts and environment variables work
  [verify:wrapix](../tests/darwin/mount-test.sh)

## Out of Scope

- Network filtering or firewall rules
- GPU passthrough
- Windows support
