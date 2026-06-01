# Core Sandbox

Platform-agnostic container isolation for coding agents — composes a workspace profile, an OCI image, and a launcher binary; runs on Linux (Podman, optionally krun-backed microVM) and macOS (Apple `container` CLI on Virtualization.framework).

## Problem Statement

Running AI coding assistants with unrestricted host access creates security risks. The sandbox must protect host filesystem and processes from container actions, preserve host UID/GID for workspace files, support outbound network for research and package management, and work consistently across Linux and macOS without per-platform consumer code.

## Architecture

`mkSandbox` is the entry point. It composes three concerns owned elsewhere and returns a four-field attrset:

- A workspace **profile** — packages, env, mounts, network allowlist, plugins (`profiles.md`)
- An OCI **image** built from the profile and the selected agent runtime layer (`image-builder.md`)
- A profile-agnostic **launcher** binary (`wrapix`)

`mkSandbox` returns `{ package, image, launcher, profile }`:

- `package` — `wrapix` wrapped with `makeWrapper`. `WRAPIX_AGENT` is set unconditionally and is **not** caller-overridable — the wrapper is bound to its built `(profile × agent)` image variant. `WRAPIX_DEFAULT_IMAGE_REF` and `WRAPIX_DEFAULT_IMAGE_SOURCE` are **caller-overridable defaults**: caller env wins; the wrapper's baked value applies only when the caller leaves the variable unset. One-shot users invoke `package` directly; orchestrators (e.g., loom) export the two image-ref vars to swap profiles per launch.
- `image` — the per-profile OCI artifact. Orchestrators that drive podman themselves read this and call `podman load`.
- `launcher` — the raw `wrapix` derivation with no env vars baked in (neither the `IMAGE` defaults nor `WRAPIX_AGENT`); orchestrators that supply image ref and agent runtime per call use this instead of `package`.
- `profile` — the resolved profile attrset after merging consumer `packages`, `mounts`, `env`, and MCP server packages.

**Platform dispatch** — `lib/sandbox/default.nix` selects the Podman launcher (`lib/sandbox/linux/`) or the Apple `container` CLI launcher (`lib/sandbox/darwin/`). Unsupported systems throw at evaluation.

**Boundary class** —

- macOS: microVM via Virtualization.framework, always
- Linux: rootless container by default; `WRAPIX_MICROVM=1` opts into `podman --runtime krun` when `/dev/kvm` is available. krun is bundled via Nix; without KVM the opt-in fails loudly rather than silently degrading.

Threat-model rationale for these choices lives in `specs/security.md`.

**Network posture** — `WRAPIX_NETWORK` selects mode at launch time. The launcher passes the mode and the merged allowlist into the container via `WRAPIX_NETWORK` / `WRAPIX_NETWORK_ALLOWLIST` env; the container's entrypoint then sets iptables OUTPUT rules when `WRAPIX_NETWORK=limit`. Allowlist contents (including the base entries every profile inherits) are owned by `profiles.md`.

- `open` (default) — unrestricted outbound; no inbound ports on either platform
- `limit` — outbound restricted to the profile's merged `networkAllowlist`. Any other value errors at the launcher before the container starts.

Filtering requires `NET_ADMIN`: a Linux rootless container has no `NET_ADMIN`, so `WRAPIX_NETWORK=limit` there logs a warning and falls back to open network. Pair `WRAPIX_NETWORK=limit` with `WRAPIX_MICROVM=1` on Linux to actually enforce. macOS runs in a microVM unconditionally, so `limit` works without further flags.

**Agent runtime axis** — the `agent` parameter selects which binary the container's entrypoint launches:

- `claude` (default) — from nixpkgs; no consumer package required
- `pi` — adds a consumer-supplied `piPkg` (e.g. loom's `pi-coding-agent`); `mkSandbox` throws when `piPkg` is missing
- `direct` — adds a consumer-supplied `directRunner` (e.g. `loom-direct-runner`); `mkSandbox` throws when `directRunner` is missing

The agent runtime composes orthogonally with the workspace profile — image variants are `(profile × agent)`, not per-profile-and-agent special cases.

**MCP servers** — `mkSandbox`'s `mcp` parameter opts servers in per sandbox (`mcp.tmux = { … }`, `mcp.playwright = { … }`). Server contracts live in their own specs (`tmux-mcp.md`, `playwright-mcp.md`). `mcpRuntime = true` bakes all registered servers into the image and defers selection to the entrypoint at run time.

## mkSandbox API

```nix
mkSandbox {
  profile = profiles.base;          # Workspace profile (profiles.md). Default: base
  cpus = null;                      # CPU limit honored by the platform launcher
  memoryMb = 4096;                  # Memory limit MB
  deployKey = "myproject";          # SSH key name — mounts host key into /etc/wrapix/keys/<name>
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
  # piPkg = ...;                    # Required when agent = "pi"; consumer-supplied (e.g. loom's pi-coding-agent)
  # directRunner = ...;             # Required when agent = "direct"; consumer-supplied (e.g. loom-direct-runner)
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

`SpawnConfig` JSON has stable top-level fields:

- `image_ref` — podman ref
- `image_source` — Nix store path the launcher loads via `podman load` before invoking podman; idempotent on the image's hash tag
- `workspace` — host path bind-mounted at `/workspace`
- `env` — allowlist of `[key, value]` pairs to pass through
- `agent_args` — argv tail passed to the agent binary
- `mounts` — optional `[{host_path, container_path, read_only}]` list; omitted or empty means no per-launch mounts. Additive to `profile.mounts` and `mkSandbox.mounts`.

Plus consumer-defined fields the entrypoint reads from inside the container. The schema is part of the wrapix CLI contract — see `wrapix spawn --help` and the parsing block in `lib/sandbox/{linux,darwin}/default.nix`.

`wrapix run` (interactive) reads the image to load from `WRAPIX_DEFAULT_IMAGE_REF` (podman ref) and `WRAPIX_DEFAULT_IMAGE_SOURCE` (Nix store path). The convenience flake outputs `packages.sandbox-<profile>` install both as caller-overridable defaults — see the `package` entry in *Architecture* for the precedence rule. Without the env populated either way, the launcher errors at startup; there is no implicit default image baked in.

## Platform Implementations

### Linux (Podman)

- `--network=pasta` userspace networking (open outbound, no inbound ports) in default container mode
- `--userns=keep-id` UID mapping so files in `/workspace` carry host UID/GID
- `--pids-limit 4096` fork-bomb guard
- `WRAPIX_MICROVM=1` switches to `podman --runtime krun` when `/dev/kvm` exists
- `WRAPIX_NETWORK=limit` enforces the merged allowlist at the launcher level

### macOS (Apple `container` CLI)

- Requires macOS 26+ and Apple Silicon
- Virtualization.framework microVM, always (no separate container-mode path)
- vmnet networking (open outbound, no inbound ports)
- VirtioFS workspace mount
- Mount classifier handles `profile.mounts` and `SpawnConfig.mounts` uniformly — directories staged + copied at launch, regular files copy-from-parent-dir, Unix-socket sources rejected at launch
- Entrypoint creates user matching host UID

## Success Criteria

- `mkSandbox` accepts the documented parameter set (`profile`, `cpus`, `memoryMb`, `deployKey`, `packages`, `mounts`, `env`, `mcp`, `mcpRuntime`, `agent`, `model`) and returns `{ package, image, launcher, profile }`
  [check](grep -nE 'profile \?|cpus \?|memoryMb \?|deployKey \?|packages \?|mounts \?|env \?|mcp \?|mcpRuntime \?|agent \?|model \?|inherit package image launcher' lib/sandbox/default.nix)
- Platform dispatch picks the Linux implementation on Linux hosts and the macOS implementation on Darwin hosts
  [check](grep -nE 'isLinux|isDarwin' lib/sandbox/default.nix)
- Evaluating `mkSandbox` on an unsupported system throws at evaluation time rather than producing a broken derivation
  [check](grep -nE 'Unsupported system' lib/sandbox/default.nix)
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
- With `NET_ADMIN` available (`WRAPIX_MICROVM=1` on Linux, microVM on macOS), `WRAPIX_NETWORK=limit` restricts outbound to the merged allowlist
  [system](bash tests/sandbox/network-modes.sh)
- Without `NET_ADMIN` (Linux rootless container), `WRAPIX_NETWORK=limit` logs a warning to stderr and falls back to open network rather than failing the launch
  [system](bash tests/sandbox/network-modes.sh)
- `WRAPIX_MICROVM=1` selects `podman --runtime krun` on Linux when `/dev/kvm` is available, and fails loudly when KVM is missing
  [check](grep -nE 'WRAPIX_MICROVM|--runtime krun|/dev/kvm' lib/sandbox/linux/default.nix)
- `wrapix run` errors at startup with a clear message when `WRAPIX_DEFAULT_IMAGE_REF` or `WRAPIX_DEFAULT_IMAGE_SOURCE` is unset
  [system](bash tests/sandbox/missing-image-env.sh)
- `mkSandbox`'s `package` wrapper installs `WRAPIX_DEFAULT_IMAGE_REF` and `WRAPIX_DEFAULT_IMAGE_SOURCE` as caller-overridable defaults via `makeWrapper --set-default` (not `--set`)
  [check](grep -nE -- '--set-default[[:space:]]+WRAPIX_DEFAULT_IMAGE_(REF|SOURCE)' lib/sandbox/default.nix)
- `mkSandbox`'s `package` wrapper installs `WRAPIX_AGENT` via unconditional `makeWrapper --set` so the wrapper's built agent runtime cannot be overridden at exec time
  [check](grep -nE -- '--set[[:space:]]+WRAPIX_AGENT' lib/sandbox/default.nix)
- When a caller pre-sets `WRAPIX_DEFAULT_IMAGE_REF` and/or `WRAPIX_DEFAULT_IMAGE_SOURCE` before exec'ing the wrapped `package`, the caller's value for each set variable reaches the underlying launcher; for any variable the caller leaves unset, the wrapper's baked default fills in
  [system](bash tests/sandbox/wrapper-image-env-override.sh)
- `wrapix spawn --spawn-config <file>` parses the documented `SpawnConfig` fields (`image_ref`, `image_source`, `workspace`, `env`, `agent_args`, `mounts`)
  [check](grep -nE 'image_ref|image_source|workspace|env|agent_args|mounts' lib/sandbox/linux/default.nix lib/sandbox/darwin/default.nix)
- On Linux, each `SpawnConfig.mounts` entry becomes a `-v <host_path>:<container_path>` podman argument, with `:ro` appended when `read_only: true`. A missing or empty `mounts` list produces no additional `-v` flags.
  [system](bash tests/sandbox/spawn-config-mounts.sh)
- On Darwin, the same mount classifier handles `profile.mounts` and `SpawnConfig.mounts` — one mechanism, not two. Directories are staged + copied at launch, regular files copy-from-parent-dir, and entries whose `host_path` is a Unix socket cause the launcher to fail loudly before the container starts. (VirtioFS does not pass socket operations, so a silently-mounted socket would dead-end at the first `connect()`.)
  [system](bash tests/sandbox/darwin-mount-classifier.sh)
- The container entrypoint switches on `WRAPIX_AGENT` and exec's the matching agent binary (`claude`, `pi`, `direct`)
  [check](grep -nE 'WRAPIX_AGENT' lib/sandbox/linux/entrypoint.sh lib/sandbox/darwin/entrypoint.sh)
- Deploy key `<name>` is mounted at `/etc/wrapix/keys/<name>` inside the container when `deployKey = "<name>"` is set (the `.pub` file is not mounted; the entrypoint regenerates it on demand via `ssh-keygen -y`)
  [check](grep -nE 'containerKeyDir|deployKey' lib/sandbox/linux/default.nix lib/sandbox/darwin/default.nix)
- `model = "<id>"` overrides `ANTHROPIC_MODEL` in the baked claude settings; null leaves the default in place
  [check](grep -nE 'ANTHROPIC_MODEL|modelEnvOverride' lib/sandbox/default.nix)
- When `/workspace/bin` exists inside the container, it appears first on `PATH`, so a consumer-supplied shim at `/workspace/bin/<name>` resolves ahead of a same-named binary baked into the image
  [system?](bash tests/sandbox/workspace-bin-path.sh)
- When `/workspace/bin` does not exist, the container's `PATH` does not contain `/workspace/bin`
  [system?](bash tests/sandbox/workspace-bin-path.sh)
- Both `lib/sandbox/linux/entrypoint.sh` and `lib/sandbox/darwin/entrypoint.sh` implement the `/workspace/bin` PATH prepend
  [check](grep -nE 'PATH="/workspace/bin:' lib/sandbox/linux/entrypoint.sh lib/sandbox/darwin/entrypoint.sh)

## Requirements

### Functional

1. **mkSandbox API** — accepts the parameters above; returns `{ package, image, launcher, profile }`. Profile schema lives in `profiles.md`; image build in `image-builder.md`; MCP server contracts in `tmux-mcp.md` and `playwright-mcp.md`.
2. **Platform dispatch** — Linux selects the Podman launcher; macOS selects the Apple `container` CLI launcher; unsupported systems throw.
3. **Workspace mount** — CWD bind-mounts at `/workspace`; profile mounts merge on top.
4. **UID mapping** — files created in `/workspace` carry host UID/GID.
5. **Custom mounts and env** — `mkSandbox`'s `mounts` and `env` extend the profile rather than replace it.
6. **Deploy keys** — `deployKey = "<name>"` mounts the host key into the container at `/etc/wrapix/keys/<name>` (and `/etc/wrapix/keys/<name>-signing` when a signing key is present). The `.pub` file is not mounted; the entrypoint regenerates it on demand via `ssh-keygen -y`. Host-source resolution and the env-first override (`WRAPIX_DEPLOY_KEY`, `WRAPIX_SIGNING_KEY`) are owned by `security.md`.
7. **MCP opt-in** — `mcp.<server>` enables a named server per `tmux-mcp.md` / `playwright-mcp.md`. `mcpRuntime = true` bakes all registered servers and defers selection to the entrypoint.
8. **Agent runtime axis** — `agent` selects the entrypoint binary; the agent runtime layer composes orthogonally with the workspace profile.
9. **Model override** — `model = "claude-…"` sets `ANTHROPIC_MODEL` in the baked `~/.claude/settings.json` env block.
10. **Launcher contract** — `wrapix run` reads `WRAPIX_DEFAULT_IMAGE_REF` and `WRAPIX_DEFAULT_IMAGE_SOURCE` from env; `wrapix spawn` reads `SpawnConfig` JSON. Both share container construction. Wrapper env-var pinning rules (which vars are caller-overridable defaults, which are unconditional) are owned by *Architecture > `package`*.
11. **Per-launch mounts via SpawnConfig** — `wrapix spawn`'s `SpawnConfig.mounts` adds per-launch bind mounts on top of `profile.mounts` and `mkSandbox`'s `mounts`. Each entry maps `host_path → container_path` with `read_only: true` rendering `:ro`. On Linux this is a literal `-v` flag. On Darwin, `SpawnConfig.mounts` flows through the same mount classifier as `profile.mounts`: directories staged + copied, regular files copy-from-parent-dir, Unix-socket sources rejected at launch with a clear error (VirtioFS does not pass socket operations). The launcher does not validate that `host_path` exists; podman fails at runtime if it does not.
12. **Workspace `bin/` PATH prepend** — When `/workspace/bin` exists inside the container, both Linux and macOS entrypoints prepend it to `PATH` so consumer-supplied shims under the workspace's `bin/` resolve ahead of image-baked binaries with the same name. The check is directory existence, not per-binary; the consumer owns what it ships in `bin/`. When the directory is absent, `PATH` is unchanged. The contract is PATH ordering only — wrapix does not create `/workspace/bin`, does not validate its contents, and does not allowlist individual shims.

### Non-Functional

1. **Rootless / no elevated privileges** — Linux runs rootless Podman; macOS runs the Apple `container` CLI as the calling user. No host capabilities granted.
2. **Boundary class** — macOS is always microVM; Linux defaults to rootless container, opts into microVM with `WRAPIX_MICROVM=1` (see `specs/security.md`).
3. **Network posture** — outbound only (no inbound ports on either platform). `WRAPIX_NETWORK=limit` enforces the merged allowlist via iptables when `NET_ADMIN` is available; see *Network posture* in Architecture for the platform availability matrix and the rootless-Linux fallback.
4. **Near-native performance** — minimal overhead beyond the container/microVM boundary cost; krun adds ~100MB per microVM.

## Out of Scope

- Windows support
- GPU passthrough
- Inbound port forwarding
- Network filtering beyond the profile allowlist mechanism (deeper firewalling lives in user-side infra)
- Per-user multi-tenant sharing (sandboxes are single-user-per-host by design)
