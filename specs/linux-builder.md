# Linux Builder

Remote Nix builder running in an Apple `container` VM on macOS, exposed via `ssh-ng://` so the host nix-daemon can dispatch `aarch64-linux` builds without leaving the local machine.

## Problem Statement

macOS users need `aarch64-linux` builds for container images, cross-platform CI, and Linux-only code paths. Apple Silicon can run Linux VMs efficiently, but wiring a Nix remote builder (persistent store, SSH keys, route configuration, nix-darwin integration) by hand is fiddly. The Linux Builder packages all of that as a single `wrapix-builder` CLI.

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

The builder runs under Apple's `container` CLI (Virtualization.framework microVM, same boundary class as wrapix sandboxes on macOS — see `specs/security.md`). `/nix` is bind-mounted from `~/.local/share/wrapix/builder-nix/` so the store persists across container restarts. SSH host and client keys live next to the store under `builder-keys/`.

### Trust Model

The container boundary is the isolation primitive; Nix's internal sandbox is disabled inside the container (`sandbox = false`) to avoid nested namespace complexity, and the `builder` user is trusted by the nix-daemon. This is appropriate because SSH binds to `127.0.0.1` only, password authentication is disabled, and the SSH key is reachable only by the host user who started the builder.

## CLI Surface

| Command | Description |
|---------|-------------|
| `wrapix-builder start` | Start builder container |
| `wrapix-builder stop` | Stop and remove container |
| `wrapix-builder status` | Show builder state |
| `wrapix-builder ssh [cmd]` | Connect or run remote command |
| `wrapix-builder setup` | Configure routes and SSH known_hosts (sudo) |
| `wrapix-builder config` | Print nix-darwin configuration snippet |

## Storage Layout

```
~/.local/share/wrapix/
├── builder-nix/      # Persistent /nix store
└── builder-keys/     # SSH keys
    ├── host_ed25519
    └── client_ed25519
```

## Setup Process

1. `wrapix-builder start` creates the container with the VirtioFS `/nix` mount; first start copies `/nix-image/*` to initialize the store
2. `wrapix-builder setup` adds the host route and SSH `known_hosts` entry (sudo required)
3. User adds `builders = ssh-ng://builder@localhost:2222 aarch64-linux` to `~/.config/nix/nix.conf` (or runs the snippet from `wrapix-builder config` in a nix-darwin module)

## Success Criteria

- The `wrapix-builder` integration suite passes on macOS 26+ (start, status, SSH, nix-daemon, remote `nixpkgs#hello` build, store persistence across `stop`/`start`, `config` snippet); skips with exit 77 on non-Darwin or older macOS
  [system](bash tests/standalone/builder-test.sh)
- sshd inside the container has `PasswordAuthentication no` and binds the listener to `127.0.0.1`
  [check](grep -nE 'PasswordAuthentication|ListenAddress' lib/sandbox/builder/entrypoint.sh)

## Requirements

### Functional

1. **Container lifecycle** — `wrapix-builder start` / `stop` / `status` manage a single Apple `container` instance named for the builder.
2. **Persistent Nix store** — `/nix` is bind-mounted from `~/.local/share/wrapix/builder-nix/`; the first `start` seeds it from the image's initial store, subsequent starts reuse it.
3. **SSH access** — sshd listens on 22 inside the container; the Apple `container` CLI forwards `127.0.0.1:2222` on the host to it. Authentication is key-based only.
4. **Route and known_hosts setup** — `wrapix-builder setup` runs sudo-required host configuration so the nix-daemon can reach the listener and trust the host key.
5. **Key management** — host and client SSH keys are generated on first run, stored under `~/.local/share/wrapix/builder-keys/`, and never regenerated unless the user opts in.
6. **nix-darwin integration** — `wrapix-builder config` emits the buildMachines snippet for use in nix-darwin modules.

### Non-Functional

1. **Minimal overhead** — uses the Apple `container` CLI's microVM directly; no extra VM management layer.
2. **Single-user design** — one builder per host user. Not suitable for multi-tenant or shared build infrastructure.
3. **Localhost only** — sshd binds to `127.0.0.1`; the builder is never reachable from the network.

## Out of Scope

- `x86_64-linux` builds (would require emulation)
- Multi-user builder access
- Remote builders over network (localhost only)
- Linux-host equivalents (the builder is the macOS workaround for cross-platform builds; Linux hosts build natively)
