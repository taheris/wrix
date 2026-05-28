# Core Sandbox

Platform-agnostic container isolation for coding agents — composes a workspace profile, an OCI image, and a launcher binary; runs on Linux (Podman, optionally krun-backed microVM) and macOS (Apple `container` CLI on Virtualization.framework).

## Problem Statement

Running AI coding assistants with unrestricted host access creates security risks. The sandbox must protect host filesystem and processes from container actions, preserve host UID/GID for workspace files, support outbound network for research and package management, and work consistently across Linux and macOS without per-platform consumer code.

## Architecture

`mkSandbox` is the entry point. It composes three concerns owned elsewhere and returns a four-field attrset:

- A workspace **profile** — packages, env, mounts, network allowlist, plugins (`profiles.md`)
- An OCI **image** built from the profile and the selected agent runtime layer (`image-builder.md`)
- A profile-agnostic **launcher** binary (`wrapix`) baked at build time with the image ref and source as defaults

`mkSandbox` returns `{ package, image, launcher, profile }`:

- `package` — `wrapix` wrapped with `makeWrapper`, with `WRAPIX_AGENT`, `WRAPIX_DEFAULT_IMAGE_REF`, and `WRAPIX_DEFAULT_IMAGE_SOURCE` baked in as defaults. One-shot users invoke this directly.
- `image` — the per-profile OCI artifact. Orchestrators that drive podman themselves read this and call `podman load`.
- `launcher` — the underlying `wrapix` derivation without the IMAGE env-var defaults; orchestrators that supply their own image ref per call use this instead of `package`.
- `profile` — the resolved profile attrset after merging consumer `packages`, `mounts`, `env`, and MCP server packages.

**Platform dispatch** — `lib/sandbox/default.nix` selects `linuxSandbox.mkSandbox` (Podman) or `darwinSandbox.mkSandbox` (Apple `container` CLI). Unsupported systems throw at evaluation.

**Boundary class** — see `docs/security-review.md` for the full analysis.

- macOS: microVM via Virtualization.framework, always
- Linux: rootless container by default; `WRAPIX_MICROVM=1` opts into `podman --runtime krun` when `/dev/kvm` is available. krun is bundled via Nix; without KVM the opt-in fails loudly rather than silently degrading.

**Network posture** — `WRAPIX_NETWORK` selects mode at launch time. The launcher passes the mode and the merged allowlist into the container via `WRAPIX_NETWORK` / `WRAPIX_NETWORK_ALLOWLIST` env; the container's entrypoint then sets iptables OUTPUT rules when `WRAPIX_NETWORK=limit`. Allowlist contents (including the base entries every profile inherits) are owned by `profiles.md`.

- `open` (default) — unrestricted outbound; no inbound ports on either platform
- `limit` — outbound restricted to the profile's merged `networkAllowlist`. Any other value errors at the launcher before the container starts.

Filtering requires `NET_ADMIN`: a Linux rootless container has no `NET_ADMIN`, so `WRAPIX_NETWORK=limit` there logs a warning and falls back to open network. Pair `WRAPIX_NETWORK=limit` with `WRAPIX_MICROVM=1` on Linux to actually enforce. macOS runs in a microVM unconditionally, so `limit` works without further flags.

**Agent runtime axis** — the `agent` parameter selects which binary the container's entrypoint launches: `claude` (default), `pi` (adds Node.js + pi-mono runtime layer), or `direct` (adds a consumer-supplied `directRunner` package such as `loom-direct-runner`). The agent runtime composes orthogonally with the workspace profile — image variants are `(profile × agent)`, not per-profile-and-agent special cases.

**MCP servers** — `mkSandbox`'s `mcp` parameter opts servers in per sandbox (`mcp.tmux = { … }`, `mcp.playwright = { … }`). Server contracts live in their own specs (`tmux-mcp.md`, `playwright-mcp.md`). `mcpRuntime = true` bakes all registered servers into the image and defers selection to the entrypoint at run time.

## mkSandbox API

```nix
mkSandbox {
  profile = profiles.base;          # Workspace profile (profiles.md). Default: base
  cpus = null;                      # CPU limit honored by the platform launcher
  memoryMb = 4096;                  # Memory limit MB
  deployKey = "myproject";          # SSH key name — mounts ~/.ssh/deploy_keys/<name>
  packages = [ pkgs.jq ];           # Extra packages merged into profile.packages
  mounts = [ {                      # Extra mounts merged into profile.mounts
    source = "~/.config";
    dest = "/home/wrapix/.config";
    mode = "ro";                    # "ro" or "rw"
  } ];
  env = { FOO = "bar"; };           # Extra env merged into profile.env
  mcp.tmux = { };                   # MCP server opt-in
  mcpRuntime = false;               # Bake ALL MCP servers, defer selection to entrypoint
  agent = "claude";                 # "claude" (default), "pi", or "direct"
  model = null;                     # Override ANTHROPIC_MODEL in claude settings
}
```

Returns `{ package, image, launcher, profile }`.

## Launcher Subcommands

The `wrapix` launcher binary is profile-agnostic. Both subcommands share container construction (mounts, env passthrough, runtime selection, deploy key, `beads-dolt` startup); they differ only in stdio and configuration source.

| Subcommand | Stdio | Configuration source | Use case |
|------------|-------|----------------------|----------|
| `wrapix run [DIR] [CMD…]` | TTY (`-it`) | Host env + CLI args | Interactive sessions, `nix run .#sandbox-<profile>` |
| `wrapix spawn --spawn-config <file> [--stdio]` | Piped or detached | JSON file (`SpawnConfig`) | Programmatic dispatch (loom; future orchestrators) |

`SpawnConfig` JSON has stable top-level fields: `image_ref` (podman ref), `image_source` (Nix store path the launcher loads via `podman load` before invoking podman; idempotent on the image's hash tag), `workspace`, `env` (allowlist of `[key, value]` pairs), `agent_args`, plus consumer-defined fields the entrypoint reads from inside the container. The schema is part of the wrapix CLI contract — see `wrapix spawn --help` and the parsing block in `lib/sandbox/{linux,darwin}/default.nix`.

`wrapix run` (interactive) has no `--spawn-config` so it reads two env vars to know which image to load: `WRAPIX_DEFAULT_IMAGE_REF` (podman ref) and `WRAPIX_DEFAULT_IMAGE_SOURCE` (Nix store path). The convenience flake outputs `packages.sandbox-<profile>` set both via `makeWrapper`; orchestrators set them programmatically from the profile-image manifest before exec. Without these vars set, `wrapix run` errors at startup — there is no implicit default image baked into the launcher.

## Platform Implementations

### Linux (Podman)

- `--network=pasta` userspace networking (open outbound, no inbound ports) in default container mode
- `--userns=keep-id` UID mapping so files in `/workspace` carry host UID/GID
- `--pids-limit 4096` fork-bomb guard
- Workspace bind-mounted at `/workspace`; profile `mounts` merged on top
- `WRAPIX_MICROVM=1` switches to `podman --runtime krun` when `/dev/kvm` exists
- `WRAPIX_NETWORK=limit` enforces the merged allowlist at the launcher level

### macOS (Apple Container CLI)

- Requires macOS 26+ and Apple Silicon
- Virtualization.framework microVM, always (no separate container-mode path)
- vmnet networking (open outbound, no inbound ports)
- VirtioFS workspace mount
- Entrypoint creates user matching host UID

## Success Criteria

- `mkSandbox` accepts the documented parameter set (`profile`, `cpus`, `memoryMb`, `deployKey`, `packages`, `mounts`, `env`, `mcp`, `mcpRuntime`, `agent`, `model`) and returns `{ package, image, launcher, profile }`
  [check](grep -nE 'profile \?|cpus \?|memoryMb \?|deployKey \?|packages \?|mounts \?|env \?|mcp \?|mcpRuntime \?|agent \?|model \?|inherit package image launcher' lib/sandbox/default.nix)
- Platform dispatch picks Linux vs macOS implementations and throws on unsupported systems
  [check](grep -nE 'isLinux|isDarwin|Unsupported system' lib/sandbox/default.nix)
- A built sandbox starts a container and exits cleanly on both Linux and macOS
  [system](bash tests/sandbox/container-starts.sh)
- Files created inside `/workspace` carry the host UID/GID, not a container-internal UID
  [system](bash tests/sandbox/uid-mapping.sh)
- Host filesystem outside `/workspace` and declared mounts is not visible inside the container
  [system](bash tests/sandbox/filesystem-isolation.sh)
- `mounts` and `env` passed to `mkSandbox` are merged into the profile and reach the container as configured
  [system](bash tests/sandbox/custom-mounts-env.sh)
- The launcher accepts `WRAPIX_NETWORK=open` and `WRAPIX_NETWORK=limit`; any other value errors before the container starts
  [check](grep -nE "WRAPIX_NETWORK must be 'open' or 'limit'" lib/sandbox/linux/default.nix lib/sandbox/darwin/default.nix)
- With `NET_ADMIN` available (`WRAPIX_MICROVM=1` on Linux, microVM on macOS), `WRAPIX_NETWORK=limit` restricts outbound to the merged allowlist; without `NET_ADMIN` (Linux rootless container), `limit` mode logs a warning and falls back to open network
  [system](bash tests/sandbox/network-modes.sh)
- `WRAPIX_MICROVM=1` selects `podman --runtime krun` on Linux when `/dev/kvm` is available, and fails loudly when KVM is missing
  [check](grep -nE 'WRAPIX_MICROVM|--runtime krun|/dev/kvm' lib/sandbox/linux/default.nix)
- `wrapix run` errors at startup with a clear message when `WRAPIX_DEFAULT_IMAGE_REF` or `WRAPIX_DEFAULT_IMAGE_SOURCE` is unset
  [system](bash tests/sandbox/missing-image-env.sh)
- `wrapix spawn --spawn-config <file>` parses the documented `SpawnConfig` fields (`image_ref`, `image_source`, `workspace`, `env`, `agent_args`)
  [check](grep -nE 'image_ref|image_source|workspace|env|agent_args' lib/sandbox/linux/default.nix lib/sandbox/darwin/default.nix)
- The container entrypoint switches on `WRAPIX_AGENT` and exec's the matching agent binary (`claude`, `pi`, `direct`)
  [check](grep -nE 'WRAPIX_AGENT' lib/sandbox/linux/entrypoint.sh lib/sandbox/darwin/entrypoint.sh)
- Deploy key `<name>` is mounted at `~/.ssh/deploy_keys/<name>{,.pub}` inside the container when `deployKey = "<name>"` is set
  [check](grep -nE 'deploy_keys|deployKey' lib/sandbox/linux/default.nix lib/sandbox/darwin/default.nix)
- `model = "<id>"` overrides `ANTHROPIC_MODEL` in the baked claude settings; null leaves the default in place
  [check](grep -nE 'ANTHROPIC_MODEL|modelEnvOverride' lib/sandbox/default.nix)

## Requirements

### Functional

1. **mkSandbox API** — accepts the parameters above; returns `{ package, image, launcher, profile }`. Profile schema lives in `profiles.md`; image build in `image-builder.md`; MCP server contracts in `tmux-mcp.md` and `playwright-mcp.md`.
2. **Platform dispatch** — Linux selects the Podman launcher; macOS selects the Apple `container` CLI launcher; unsupported systems throw.
3. **Workspace mount** — CWD bind-mounts at `/workspace`; profile mounts merge on top.
4. **UID mapping** — files created in `/workspace` carry host UID/GID.
5. **Custom mounts and env** — `mkSandbox`'s `mounts` and `env` extend the profile, they do not replace.
6. **Deploy keys** — `deployKey = "<name>"` mounts `~/.ssh/deploy_keys/<name>{,.pub}` into the container's `~/.ssh/`.
7. **MCP opt-in** — `mcp.<server>` enables a named server per `tmux-mcp.md` / `playwright-mcp.md`. `mcpRuntime = true` bakes all registered servers and defers selection to the entrypoint.
8. **Agent runtime axis** — `agent` selects the entrypoint binary; the agent runtime layer composes orthogonally with the workspace profile.
9. **Model override** — `model = "claude-…"` sets `ANTHROPIC_MODEL` in the baked `~/.claude/settings.json` env block.
10. **Launcher contract** — `wrapix run` reads `WRAPIX_DEFAULT_IMAGE_REF` and `WRAPIX_DEFAULT_IMAGE_SOURCE` from env; `wrapix spawn` reads `SpawnConfig` JSON. Both share container construction.

### Non-Functional

1. **Rootless / no elevated privileges** — Linux runs rootless Podman; macOS runs the Apple `container` CLI as the calling user. No host capabilities granted.
2. **Boundary class** — macOS is always microVM; Linux defaults to rootless container, opts into microVM with `WRAPIX_MICROVM=1` (see `docs/security-review.md`).
3. **Network posture** — `WRAPIX_NETWORK=open` (default) permits unrestricted outbound; `WRAPIX_NETWORK=limit` enforces the merged allowlist via iptables in the entrypoint. Filtering requires `NET_ADMIN` (microVM on macOS, `WRAPIX_MICROVM=1` on Linux); without it, `limit` falls back to open network with a stderr warning. No inbound ports on either platform.
4. **Near-native performance** — minimal overhead beyond the container/microVM boundary cost; krun adds ~100MB per microVM as documented in `docs/security-review.md`.

## Out of Scope

- Windows support
- GPU passthrough
- Inbound port forwarding
- Network filtering beyond the profile allowlist mechanism (deeper firewalling lives in user-side infra)
- Per-user multi-tenant sharing (sandboxes are single-user-per-host by design)
