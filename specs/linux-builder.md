# Linux Builder

Remote Nix builder running in an Apple `container` VM on macOS, exposed via `ssh-ng://` so the host nix-daemon can dispatch native-architecture Linux builds without leaving the local machine.

## Problem Statement

macOS users need Linux builds for container images, cross-platform CI, and Linux-only code paths. Macs can run same-architecture Linux VMs efficiently, but wiring a Nix remote builder (persistent store, SSH keys, route configuration, nix-darwin integration) by hand is fiddly. The Linux Builder packages all of that as a single `wrix-builder` CLI.

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
                                   /nix (ext4 volume)
                                       │
Apple volume: wrix-builder-nix ◄─────┘
```

The builder runs under Apple's `container` CLI (Virtualization.framework microVM, same boundary class as wrix sandboxes on macOS — see `specs/security.md`). Its image system follows the macOS host architecture: `aarch64-linux` on Apple Silicon and `x86_64-linux` on Intel. `/nix` is backed by the case-sensitive Apple volume `wrix-builder-nix`, so the store persists across container restarts without passing Linux store filenames through macOS's case-insensitive filesystem. SSH host and client keys remain under the host user's `builder-keys/` directory.

### Trust Model

The container boundary is the isolation primitive; Nix's internal sandbox is disabled inside the container (`sandbox = false`) to avoid nested namespace complexity, and the `builder` user is trusted by the nix-daemon. The host publishes SSH on `127.0.0.1` only, while sshd listens on the guest's IPv4 interfaces so Apple's port forwarding can reach it. Password authentication is disabled, and the SSH key is reachable only by the host user who started the builder.

### Image-source contract

The `wrix-builder` bootstrap image follows the support-image metadata and label contract owned by `image-builder.md`. The packaged CLI uses that metadata to choose the load transport before invoking Apple's `container image load`; this spec owns the lifecycle and persistent-store seeding.

## CLI Surface

| Command | Description |
|---------|-------------|
| `wrix-builder start` | Start builder container |
| `wrix-builder stop` | Stop and remove container |
| `wrix-builder status` | Show builder state |
| `wrix-builder ssh [cmd]` | Connect or run remote command |
| `wrix-builder setup` | Configure routes and SSH known_hosts (sudo) |
| `wrix-builder config` | Print nix-darwin configuration snippet |

## Storage Layout

```
~/.local/share/wrix/
├── builder-keys/                 # SSH keys
│   ├── host_ed25519
│   └── client_ed25519
└── builder-volume-image-version  # Verified image version for the volume

Apple container volumes:
└── wrix-builder-nix              # 40 GB ext4 volume mounted at /nix
```

The former `~/.local/share/wrix/builder-nix/` bind store is never mounted or deleted automatically. If present, `wrix-builder` reports it as preserved legacy data so the user can remove it after validating the new volume.

## Setup Process

1. On Darwin, `wrix-builder start` creates the labelled ext4 volume on first use and streams the bootstrap closure through Nix's canonical export/import format into a chrooted store on that volume
2. The import is accepted only after `nix-store --verify --check-contents` succeeds; the builder then mounts the volume at `/nix`
3. `start` repairs a conflicting VPN route before waiting for authenticated SSH, while `wrix-builder setup` installs the client identity under `/etc/nix` and adds the SSH host key to root's `known_hosts` (sudo required)
4. User adds `builders = ssh-ng://builder@localhost:2222 <native-linux-system> /etc/nix/wrix_builder_ed25519 4 1 big-parallel,benchmark` to `~/.config/nix/nix.conf` or uses the pure nix-darwin module printed by `wrix-builder config`

## Success Criteria

- The `wrix-builder` integration suite passes on macOS 26+ (`start` repairs routes before waiting for nix-daemon and authenticated SSH, the imported store passes full content verification, status, remote `nixpkgs#hello` build using the generated native-system configuration, store persistence across `stop`/`start`, pure `config` snippet evaluation); skips with exit 77 on non-Darwin or older macOS
  [system](verify:linux-builder.integration)
- The host publishes SSH on `127.0.0.1:2222` only, while sshd has `PasswordAuthentication no` and listens on guest `0.0.0.0:22` so Apple port forwarding can reach it
  [check](test-ci:test-linux-builder-sshd-hardening)
- Builder host and client SSH keys are generated under the host user's `~/.local/share/wrix/builder-keys/` directory with private keys mode `600`
  [system](verify:linux-builder.key-material-generation)
- Re-running builder key-material initialization preserves existing private keys
  [system](verify:linux-builder.key-material-idempotent)
- `wrix-builder start` routes the bootstrap image through the `source_kind` load transport before invoking Apple's `container image load`
  [system](test-ci:test-linux-builder-source-kind-load-transport)

## Requirements

### Functional

1. **Container lifecycle** — `wrix-builder start` / `stop` / `status` manage a single Apple `container` instance named for the builder.
2. **Persistent Nix store** — on Darwin, `/nix` is mounted from the labelled `wrix-builder-nix` ext4 volume; the first `start` populates it with canonical Nix export/import and verifies every registered path, while subsequent starts reuse the verified volume.
3. **SSH access** — sshd listens on `0.0.0.0:22` inside the isolated builder VM; the Apple `container` CLI forwards `127.0.0.1:2222` on the host to it. Authentication is key-based only.
4. **Route and known_hosts setup** — `wrix-builder setup` runs sudo-required host configuration so the nix-daemon can reach the listener and trust the host key.
5. **Key management** — host and client SSH keys are generated on first run, stored under `~/.local/share/wrix/builder-keys/`, and never regenerated unless the user opts in.
6. **nix-darwin integration** — `wrix-builder config` emits a pure buildMachines snippet for the native Linux system, using the identity installed by `wrix-builder setup`.

### Non-Functional

1. **Minimal overhead** — uses the Apple `container` CLI's microVM directly; no extra VM management layer.
2. **Single-user design** — one builder per host user. Not suitable for multi-tenant or shared build infrastructure.
3. **Localhost only** — the host-side SSH publication binds to `127.0.0.1`; no wildcard or external host address exposes the builder.

## Out of Scope

- Cross-architecture or emulated Linux builds
- Multi-user builder access
- Remote builders over network (localhost only)
- Linux-host equivalents (the builder is the macOS workaround for cross-platform builds; Linux hosts build natively)
