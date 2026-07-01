# Linux Builder

Remote Nix builder running in an Apple `container` VM on macOS, exposed via `ssh-ng://` so the host nix-daemon can dispatch `aarch64-linux` builds without leaving the local machine.

## Problem Statement

macOS users need `aarch64-linux` builds for container images, cross-platform CI, and Linux-only code paths. Apple Silicon can run Linux VMs efficiently, but wiring a Nix remote builder (persistent store, SSH keys, route configuration, nix-darwin integration) by hand is fiddly. The Linux Builder packages all of that as a single `wrix-builder` CLI.

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
~/.local/share/wrix/builder-nix/ ◄───┘
```

The builder runs under Apple's `container` CLI (Virtualization.framework microVM, same boundary class as wrix sandboxes on macOS — see `specs/security.md`). `/nix` is bind-mounted from `~/.local/share/wrix/builder-nix/` so the store persists across container restarts. SSH host and client keys live next to the store under `builder-keys/`.

### Trust Model

The container boundary is the isolation primitive; Nix's internal sandbox is disabled inside the container (`sandbox = false`) to avoid nested namespace complexity, and the `builder` user is trusted by the nix-daemon. This is appropriate because SSH binds to `127.0.0.1` only, password authentication is disabled, and the SSH key is reachable only by the host user who started the builder.

### Image-source contract

The `wrix-builder` bootstrap image is a wrix-managed support image consumed by the macOS builder. It exposes the shared `{ ref, source, source_kind, digest }` image-source contract and retains wrix ownership labels. The packaged CLI uses the contract fields to choose the source-kind load transport before invoking Apple's `container image load`; this spec owns that lifecycle and persistent-store seeding, while `image-builder.md` owns the source-kind metadata and label schema.

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
├── builder-nix/      # Persistent /nix store
└── builder-keys/     # SSH keys
    ├── host_ed25519
    └── client_ed25519
```

## Setup Process

1. `wrix-builder start` creates the container with the VirtioFS `/nix` mount; first start copies `/nix-image/*` to initialize the store
2. `wrix-builder setup` adds the host route and SSH `known_hosts` entry (sudo required)
3. User adds `builders = ssh-ng://builder@localhost:2222 aarch64-linux` to `~/.config/nix/nix.conf` (or runs the snippet from `wrix-builder config` in a nix-darwin module)

## Success Criteria

- The `wrix-builder` integration suite passes on macOS 26+ (start, status, SSH, nix-daemon, remote `nixpkgs#hello` build, store persistence across `stop`/`start`, `config` snippet); skips with exit 77 on non-Darwin or older macOS
  [system](bash tests/standalone/builder-test.sh)
- sshd inside the container has `PasswordAuthentication no` and binds the listener to `127.0.0.1`
  [check](bash -c 'grep -Fxq "PasswordAuthentication no" lib/sandbox/builder/entrypoint.sh && grep -Fxq "ListenAddress 127.0.0.1" lib/sandbox/builder/entrypoint.sh')
- Builder host and client SSH keys are generated under the host user's `~/.local/share/wrix/builder-keys/` directory with private keys mode `600`
  [system](bash tests/builder/key-material.sh test_generates_per_user_ed25519_material)
- Re-running builder key-material initialization preserves existing private keys
  [system](bash tests/builder/key-material.sh test_preserves_existing_private_keys)
- The `wrix-builder` bootstrap image exposes the shared image-source contract while retaining wrix ownership labels
  [system](bash tests/test-app.sh test-wrix-images-source-kind)

- `wrix-builder start` routes the bootstrap image through the `source_kind` load transport before invoking Apple's `container image load`
  [system](bash tests/builder/key-material.sh test_loads_image_through_source_kind_contract)

## Requirements

### Functional

1. **Container lifecycle** — `wrix-builder start` / `stop` / `status` manage a single Apple `container` instance named for the builder.
2. **Persistent Nix store** — `/nix` is bind-mounted from `~/.local/share/wrix/builder-nix/`; the first `start` seeds it from the image's initial store, subsequent starts reuse it.
3. **SSH access** — sshd listens on 22 inside the container; the Apple `container` CLI forwards `127.0.0.1:2222` on the host to it. Authentication is key-based only.
4. **Route and known_hosts setup** — `wrix-builder setup` runs sudo-required host configuration so the nix-daemon can reach the listener and trust the host key.
5. **Key management** — host and client SSH keys are generated on first run, stored under `~/.local/share/wrix/builder-keys/`, and never regenerated unless the user opts in.
6. **nix-darwin integration** — `wrix-builder config` emits the buildMachines snippet for use in nix-darwin modules.

### Non-Functional

1. **Minimal overhead** — uses the Apple `container` CLI's microVM directly; no extra VM management layer.
2. **Single-user design** — one builder per host user. Not suitable for multi-tenant or shared build infrastructure.
3. **Localhost only** — sshd binds to `127.0.0.1`; the builder is never reachable from the network.

## Out of Scope

- `x86_64-linux` builds (would require emulation)
- Multi-user builder access
- Remote builders over network (localhost only)
- Linux-host equivalents (the builder is the macOS workaround for cross-platform builds; Linux hosts build natively)
