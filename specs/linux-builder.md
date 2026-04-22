# Linux Builder

Remote Nix builds for macOS via Linux container.

## Problem Statement

macOS users need to build `aarch64-linux` packages for:
- Container images that run Linux
- Cross-platform CI/CD pipelines
- Testing Linux-specific code

Apple Silicon Macs can run Linux VMs efficiently, but setting up a Nix remote builder is complex.

## Requirements

### Functional

1. **Container Lifecycle** - Start/stop Linux builder container
2. **Persistent Nix Store** - `/nix` survives container restarts
3. **SSH Access** - Remote builds via `ssh-ng://` protocol
4. **Route Configuration** - Network setup for nix-daemon access
5. **Key Management** - Automatic SSH key generation and trust
6. **nix-darwin Integration** - Config snippet for permanent setup

### Non-Functional

1. **Minimal Overhead** - Uses Apple container CLI (lightweight VM)
2. **Automatic Initialization** - First start copies initial Nix store
3. **Secure** - SSH keys stored in user data directory

### Security

1. **SSH Port Binding** - SSH port 2222 is bound to localhost (127.0.0.1) only, not exposed to network interfaces
2. **Key-Based Auth** - Password authentication is disabled; only SSH key authentication is accepted
3. **Limited Access** - Only the `builder` user has SSH access to the container

### Trust Model

The Linux builder uses a **container-boundary security model**:

1. **Nix Sandbox Disabled** (`sandbox = false`)
   - Nix's internal build sandboxing is disabled inside the container
   - The outer container provides the security boundary instead
   - This avoids nested namespace complexity and improves build compatibility

2. **Trusted Users** (`trusted-users = root builder`)
   - The `builder` user has full trust within the Nix daemon
   - This allows remote builds to set arbitrary derivation options
   - Appropriate because:
     - SSH access is restricted to localhost (no network exposure)
     - SSH key is required (no password auth)
     - Only the host user who started the builder has the SSH key

3. **Single-User Design**
   - The builder is designed for single-user local development
   - Not suitable for multi-tenant or shared build infrastructure
   - Each user should run their own builder instance

**Security Boundary Summary:**

| Layer | Protection |
|-------|------------|
| Container | Process isolation, filesystem namespace, network namespace |
| SSH | Key-based auth only, localhost binding, no root login |
| Nix | Delegated to container boundary (sandbox disabled internally) |

This model trades Nix's internal sandboxing for container-level isolation, which is appropriate when:
- The builder runs locally on a single-user workstation
- Network access is restricted to localhost
- The user trusts code they're building (their own projects)

## Architecture

```
macOS Host                         Linux Builder Container
----------                         -----------------------
nix-daemon                         sshd (:22)
    │                                  │
    └─ ssh-ng://builder@localhost:2222 ┘
                                       │
                                   nix-daemon
                                       │
                                   /nix (VirtioFS mount)
                                       │
~/.local/share/wrapix/builder-nix/ ◄───┘
```

## Commands

| Command | Description |
|---------|-------------|
| `wrapix-builder start` | Start builder container |
| `wrapix-builder stop` | Stop and remove container |
| `wrapix-builder status` | Show builder state |
| `wrapix-builder ssh [cmd]` | Connect or run remote command |
| `wrapix-builder setup` | Configure routes and SSH (sudo) |
| `wrapix-builder config` | Print nix-darwin config snippet |

## Storage Layout

```
~/.local/share/wrapix/
├── builder-nix/      # Persistent /nix store
└── builder-keys/     # SSH keys
    ├── host_ed25519
    └── client_ed25519
```

## Setup Process

1. `wrapix-builder start` - Creates container with VirtioFS mount
2. First start copies `/nix-image/*` to initialize store
3. `wrapix-builder setup` - Adds route and SSH known_hosts (sudo)
4. Add to `~/.config/nix/nix.conf`:
   ```
   builders = ssh-ng://builder@localhost:2222 aarch64-linux
   ```

## Affected Files

| File | Role |
|------|------|
| `lib/builder/default.nix` | CLI script and package |
| `lib/builder/hostkey.nix` | SSH key generation |
| `lib/sandbox/builder/image.nix` | Builder container image |
| `lib/sandbox/builder/entrypoint.sh` | Builder startup script |

## Success Criteria

- [ ] `nix build --system aarch64-linux` works on macOS
  [judge](../tests/judges/linux-builder.sh#test_aarch64_linux_builds)
- [ ] Nix store persists across container restarts
  [judge](../tests/judges/linux-builder.sh#test_nix_store_persistence)
- [ ] SSH connection is secure (key-based auth)
  [judge](../tests/judges/linux-builder.sh#test_ssh_key_auth)
- [ ] `wrapix-builder config` prints nix-darwin configuration snippet (full nix-darwin module not yet implemented)
  [judge](../tests/judges/linux-builder.sh#test_nix_darwin_config)
- [ ] Builder can be stopped and restarted cleanly
  [judge](../tests/judges/linux-builder.sh#test_stop_restart_clean)

## Out of Scope

- x86_64-linux builds (would require emulation)
- Multi-user builder access
- Remote builders over network (localhost only)
