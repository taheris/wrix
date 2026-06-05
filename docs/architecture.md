# Architecture

Wrapix is a secure sandbox for running AI coding agents in isolated containers.
It provides container isolation on Linux (Podman) and macOS (Apple container
CLI), with built-in support for [Claude Code](https://claude.ai/code) (from
nixpkgs) and two consumer-supplied agent slots (*pi* and *direct*) for
external orchestrators such as [Loom](https://github.com/taheris/loom), plus
tooling for notifications, remote Nix builds, and integration hooks.

## Design Principles

1. **Container isolation is the security boundary** — Filesystem and process isolation protect the host
2. **Least privilege** — Containers run without elevated capabilities
3. **User namespace mapping** — Files created in `/workspace` have correct host ownership
4. **Open network** — Full internet access for web research, git, package managers
5. **Nix all the way down** — Config, images, and the launcher are deterministic Nix outputs
6. **Agent-runtime axis is orthogonal to the profile axis** — `claude`/`pi`/`direct` compose with `base`/`rust`/`python` rather than multiplying out

## Platform Support

| Platform | Container Technology | Networking |
|----------|---------------------|------------|
| Linux | Podman rootless | pasta (userspace TCP/IP) |
| macOS | Apple container CLI + Virtualization.framework | vmnet bridge |

Both platforms provide full network connectivity without elevated privileges. The `/workspace` directory is mounted read-write with correct file ownership.

## Source Layout

```
lib/
├── default.nix          # Top-level API: mkSandbox, profiles, mkProfileImages
├── sandbox/             # Container isolation
│   ├── default.nix      # Platform dispatcher, MCP integration, mkProfileImages
│   ├── profiles.nix     # Built-in profiles (base, rust, python)
│   ├── image.nix        # OCI image builder; selects agent runtime layer
│   ├── manifest.nix     # Profile→image JSON manifest consumed by orchestrators
│   ├── linux/           # Podman implementation + krun microVM support
│   ├── darwin/          # Apple container implementation
│   └── builder/         # Static-busybox bootstrap entrypoint for the Linux builder
├── beads/               # Per-workspace beads-dolt container management
├── mcp/                 # MCP server registry
│   ├── default.nix      # Server registry: { tmux, playwright }
│   ├── tmux/            # tmux MCP server
│   └── playwright/      # Playwright MCP server
├── prek/                # Pre-commit hook shims
├── builder/             # macOS-side CLI for the Linux remote builder
├── notify/              # Desktop notifications
└── util/                # Shared utilities (container CLI shim, SSH, paths, …)

docs/
├── README.md            # Project overview, terminology
├── architecture.md      # This file
├── spec-conventions.md  # Spec-authoring conventions
└── style-rules.md       # Code standards (SH-, NX-, DOC-, GIT-, TST-, RS-, COM-, CLI-)
```

## Component Overview

| Component | Purpose | Entry Point |
|-----------|---------|-------------|
| Sandbox | Container creation and lifecycle | `mkSandbox`, `wrapix run`/`spawn` |
| Profiles | Pre-configured dev environments | `profiles.{base,rust,python}` |
| Agent Runtime | Single agent binary baked into the image and exec'd by the entrypoint | `mkSandbox { agent = … }` / `sandbox-<profile>[-<agent>]` |
| MCP Servers | Optional capabilities exposed to the agent | `mcp.tmux`, `mcp.playwright`, `mcpRuntime = true` |
| Image Builder | OCI image generation via Nix | `lib/sandbox/image.nix` |
| Profile Manifest | JSON map of profile → `{ref, source}` for orchestrators | `packages.profile-images` (`mkProfileImages`) |
| Notifications | Desktop alerts when the agent waits | `wrapix-notify`, `wrapix-notifyd` |
| Linux Builder | Remote Nix builds on macOS | `wrapix-builder` |

## Sandbox Launcher

The launcher and the OCI image are separate Nix outputs, composed at the consumer's discretion:

| Output | Role |
|--------|------|
| `packages.wrapix` | Profile-agnostic launcher binary; reads image ref/source at runtime |
| `packages.image-<profile>` | Per-profile OCI artifact built with `agent = "claude"` (the default) |
| `packages.sandbox-<profile>` | `makeWrapper` of the launcher with image ref/source + `WRAPIX_AGENT=claude` baked in — the user-facing `nix run .#sandbox-rust` target. Consumers needing `pi` or `direct` build their own wrappers via `mkSandbox` |
| `packages.profile-images` | JSON manifest mapping profile → `{ref, source}`, for orchestrators that look up images by profile name |

The launcher exposes two subcommands sharing the same container construction (mounts, env passthrough, deploy key):

- `wrapix run [DIR] [CMD…]` — interactive (TTY). Reads `WRAPIX_DEFAULT_IMAGE_REF`/`WRAPIX_DEFAULT_IMAGE_SOURCE` from the env. The `sandbox-<profile>` wrappers set both; orchestrators export them from the profile-image manifest.
- `wrapix spawn --spawn-config <file> [--stdio]` — programmatic dispatch. Reads image ref/source plus workspace, env allowlist, and agent args from a JSON `SpawnConfig`. `--stdio` adds `WRAPIX_STDIO=1` so claude runs in `--input-format stream-json` mode.

See [specs/sandbox.md](../specs/sandbox.md) and [specs/profiles.md](../specs/profiles.md) for the full launcher and manifest contracts.

## Agent Runtimes

The `agent` parameter selects, **at build time**, the single agent binary the
image bakes and the entrypoint execs after staging `/workspace`, settings, and
SSH credentials — selection is by build target, not by env var. A human picks
an agent by choosing the `mkSandbox { agent = …; }` build / its
`sandbox-<profile>[-<agent>]` target. `WRAPIX_AGENT` is the internal
build→entrypoint wire, not a caller knob: the `package` wrapper pins it with
`makeWrapper --set` (non-overridable). Orchestrators driving the raw `launcher`
(which bakes no agent) set `WRAPIX_AGENT` per call, paired with a matching
per-call image.

| Value | Behaviour |
|-------|-----------|
| `claude` *(default)* | Interactive `claude` TTY, or `claude --print --input-format stream-json` when `WRAPIX_STDIO=1` |
| `pi` | `pi --mode rpc` — JSONL RPC on stdio. Requires `agent = "pi"` and a consumer-supplied `piPkg` at image-build time. |
| `direct` | Execs `loom-direct-runner` (or any consumer-named binary). Requires `agent = "direct"` and `directRunner = …` at image-build time. |

Exactly one agent rides each image — a non-claude image carries no
`claude-code` (`agent = "direct"` bakes neither `claude-code` nor `pi`). Only
the `claude` runtime is installed in the default `packages.image-<profile>`;
`pi` and `direct` are build-time selections because their binaries come from
outside wrapix. Before exec, the entrypoint verifies the selected agent's
binary is present (`command -v`) and fails loudly when it is absent from the
image — e.g. `WRAPIX_AGENT=pi` against a claude image on the raw-launcher
path — rather than emitting a bare `command not found`.

### Direct mode (orchestrator integration)

`mkSandbox { agent = "direct"; directRunner = ...; }` is the integration
seam for external orchestrators. The orchestrator provides its own Linux
binary (e.g. Loom's `loom-direct-runner`) and drives the container over
JSONL stdio. Wrapix doesn't ship its own runner — see
[Loom's flake](https://github.com/taheris/loom) for the canonical wiring.

## Security Model

**Protected**: Filesystem (only `/workspace` accessible), processes (isolated), user namespace (correct UID), capabilities (none elevated)

**Not protected**: Network traffic is unrestricted by design for autonomous work.

### MicroVM Boundary (Linux)

On Linux with KVM, containers can optionally run inside a [libkrun](https://github.com/containers/libkrun) microVM (`podman --runtime krun`) for hardware-level isolation. Set `WRAPIX_MICROVM=1` to opt in.

See [`specs/security.md`](../specs/security.md) for the full threat model.

## MCP Integration

MCP servers extend sandbox capabilities. The `mcp` parameter in `mkSandbox` accepts a set of server names:

```nix
mkSandbox {
  profile = profiles.rust;
  mcp.tmux = { };
  mcp.playwright = { };
}
```

`mcpRuntime = true` bundles every registered server into the image and lets
`WRAPIX_MCP=<csv>` pick at container start.

See [tmux-mcp.md](../specs/tmux-mcp.md) and [playwright-mcp.md](../specs/playwright-mcp.md) for details.

## State Layout

All wrapix state lives under `.wrapix/` in the host workspace, mounted at
`/workspace/.wrapix/` inside the container:

```
.wrapix/
├── log/             # Session transcripts (one JSON file per session)
├── push-verified    # Touched by lib/prek/hooks/pre-push on green nix flake check
└── dolt.sock        # Per-workspace beads-dolt server socket
```

External orchestrators (e.g. Loom) may keep their own state under
`.wrapix/<name>/` — wrapix itself doesn't manage that.
