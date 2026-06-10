# Core Sandbox

Platform-agnostic container isolation for coding agents — composes a workspace profile, an OCI image, and a launcher binary; runs on Linux (Podman, optionally krun-backed microVM) and macOS (Apple `container` CLI on Virtualization.framework).

## Problem Statement

Running AI coding assistants with unrestricted host access creates security risks. The sandbox must protect host filesystem and processes from container actions, preserve host UID/GID for workspace files, support outbound network for research and package management, and work consistently across Linux and macOS without per-platform consumer code.

## Architecture

`mkSandbox` is the entry point. It composes three concerns owned elsewhere and returns a four-field attrset:

- A workspace **profile** — packages, env, mounts, network allowlist, plugins (`profiles.md`)
- An OCI **image** built from the profile and the selected agent runtime (`image-builder.md`)
- A profile-agnostic **launcher** binary (`wrix`)

`mkSandbox` returns `{ package, image, launcher, profile }`:

- `package` — `wrix` wrapped with an immutable Nix-store `ProfileConfig` JSON path. The wrapper is bound to its built `(profile × agent)` image variant; agent selection and image defaults come from the JSON, not mutable shell logic. One-shot users invoke `package` directly.
- `image` — the per-profile OCI artifact. Orchestrators that drive the install themselves read this and feed it through their platform's install transport (see *Image install path* below).
- `launcher` — the raw Rust `wrix` derivation. Orchestrators (e.g. loom) pass `--profile-config <store-path>` and, for `spawn`, a per-launch `SpawnConfig` JSON.
- `profile` — the resolved profile attrset after merging consumer `packages`, `mounts`, `env`, and MCP server packages.

**Platform dispatch** — `lib/sandbox/default.nix` selects the Podman launcher (`lib/sandbox/linux/`) or the Apple `container` CLI launcher (`lib/sandbox/darwin/`). Unsupported systems throw at evaluation.

**Image install path** — Before invoking the platform install pipeline, the launcher checks whether the image's **content digest** (manifest digest, not ref-name+tag) matches any image already present in the platform store. On a digest hit, the install is skipped entirely — no tar materialization, no stream invocation, no `*-load` CLI call. On a miss, the launcher installs the image:

- Linux uses a per-layer-blob-dedup transport (`skopeo copy oci-archive: → containers-storage:`) so unchanged layer blobs in a re-emitted image are not re-extracted into the store.
- Darwin uses `container image load --input <tar>` (Apple's `container` CLI surfaces no per-blob-dedup install path at this time; see `image-builder.md` § Out of Scope).

Both platforms rely on the provenance-tiered `fromImage` chain (see `image-builder.md` § Provenance-Tiered Layering) to keep the per-profile leaf tar small, so even the Darwin path transfers a bounded delta rather than the full image on small input changes.

**Boundary class** —

- macOS: microVM via Virtualization.framework, always
- Linux: rootless container by default; `WRIX_MICROVM=1` opts into `podman --runtime krun` when `/dev/kvm` is available. krun is bundled via Nix; without KVM the opt-in fails loudly rather than silently degrading.

Threat-model rationale for these choices lives in `specs/security.md`.

**Network posture** — `WRIX_NETWORK` selects public egress posture at launch time. The launcher passes the mode, merged allowlist, DNS exceptions, and wrix-owned local endpoint exceptions into the container via env/config; the entrypoint installs a Linux in-sandbox firewall ruleset before the agent starts. Linux Podman uses `nftables` by default. Darwin does not use host `pf`; it uses the firewall backend available inside the Linux guest/container (`nftables` when supported, otherwise a verified equivalent such as iptables).

Baseline network isolation is always enforced in both modes: no inbound ports, IPv6 disabled/blocked for v1, and outbound traffic to LAN/private/host-local/VPN/special ranges is blocked. Exact exceptions are allowed only for wrix-owned endpoints (for example the project cache host-gateway IP/port, Darwin Dolt TCP endpoint) and configured DNS resolvers on TCP/UDP port 53.

- `open` (default) — public-internet outbound is allowed, but LAN/private/host-local/VPN/special ranges remain blocked.
- `limit` — outbound is restricted to the profile's merged `networkAllowlist` plus exact wrix-owned local endpoint and DNS exceptions; LAN/private/host-local/VPN/special ranges remain blocked. Any other value errors at the launcher before the container starts.

Filtering is fail-closed. Linux rootless Podman grants temporary in-container `NET_ADMIN` so the entrypoint can install namespace-local firewall rules atomically, then uses `capsh` to drop `NET_ADMIN` before execing the agent. `WRIX_MICROVM=1` remains an optional stronger boundary, not a requirement. macOS runs in a microVM unconditionally and applies the same baseline/limit policy inside that boundary without mutating the Darwin host firewall. `WRIX_NETWORK=limit` domains are resolved once at startup; any unresolvable allowlist domain fails launch instead of being silently omitted. If firewall setup, IPv6 disablement, or capability drop cannot be verified, launch fails; wrix never falls back to LAN-open networking.

**Agent runtime axis** — the `agent` parameter selects, **at build time**, the single agent binary the image bakes and the entrypoint launches. The binary must be in the image, so this is not a runtime knob.

- `direct` (default) — default base image; consumers can override the placeholder with `agentPkg`
- `claude` — `claude-code` from nixpkgs; no consumer package required
- `pi` — `pi-coding-agent` from nixpkgs by default

Exactly one agent rides each image — `agent = "direct"` bakes neither `claude-code` nor `pi`. The agent runtime is its own image tier (`image-builder.md` § Provenance-Tiered Layering); it composes orthogonally with the profile, so variants are `(profile × agent)`.

**Selection is by build target, not by caller env.** `WRIX_AGENT` is the internal wire the entrypoint reads, but callers do not select it by exporting env vars. A human selects an agent by choosing the `mkSandbox { agent = …; }` build / its `sandbox-<profile>[-<agent>]` target; that choice is encoded in the immutable `ProfileConfig` JSON. Orchestrators driving the raw `launcher` pass a matching per-call `ProfileConfig`.

**Entrypoint binary guard.** The entrypoint dispatches on `WRIX_AGENT` and, before exec, verifies the named binary is present (`command -v`). A request for an agent absent from the image — e.g. `WRIX_AGENT=pi` against a claude image on the raw-launcher path — fails loudly with a clear error instead of a bare `command not found`.

**Per-agent configuration is delivered, not abstracted.** Each agent keeps its own config system; wrix only delivers config to it:

- *Config home* — the entrypoint seeds the agent's config home from baked defaults and persists session data via `/workspace`, per agent: claude → `~/.claude`, pi → `~/.pi/agent`; `direct` has none.
- *Credentials* — API keys and OAuth/subscription tokens reach the agent via ordinary env passthrough (`env`, host env, `SpawnConfig.env`) and mounts; secrets are never baked into the image. The credential invariants are owned by `security.md`.
- *Package/settings overrides* — `agentPkg` overrides the selected agent package; `agentSettings` merges into the selected agent's settings schema. `agentSettings` is rejected for `agent = "direct"` until direct has a settings schema.

**MCP servers** — `mkSandbox`'s `mcp` parameter opts servers in per sandbox (`mcp.tmux = { … }`, `mcp.playwright = { … }`). Server contracts live in their own specs (`tmux-mcp.md`, `playwright-mcp.md`). `mcpRuntime = true` bakes all registered servers into the image and defers selection to the entrypoint at run time.

## mkSandbox API

```nix
mkSandbox {
  profile = profiles.base;          # Workspace profile (profiles.md). Default: base
  cpus = null;                      # CPU limit honored by the platform launcher
  memoryMb = 4096;                  # Memory limit MB
  deployKey = "myproject";          # SSH key name — mounts host key into /etc/wrix/keys/<name>
  packages = [ pkgs.jq ];           # Extra packages merged into profile.packages
  mounts = [ {                      # Extra mounts merged into profile.mounts
    source = "~/.config";
    dest = "/home/wrix/.config";
    mode = "ro";                    # "ro" or "rw"
  } ];
  env = { FOO = "bar"; };           # Extra env merged into profile.env
  mcp.tmux = { };                   # MCP server opt-in
  mcpRuntime = false;               # Bake ALL MCP servers, defer selection to entrypoint
  agent = "direct";                 # "direct" (default), "claude", or "pi"
  agentPkg = null;                  # Optional selected-agent package override
  agentSettings = { };              # Settings for the selected agent
}
```

Returns `{ package, image, launcher, profile }`.

## Launcher Subcommands

The Rust `wrix` launcher binary is profile-agnostic. Nix supplies build-time defaults through an immutable `ProfileConfig` JSON file, passed by `--profile-config <path>` or a wrapper-set equivalent. Both launcher subcommands share container construction (mounts, env passthrough, runtime selection, deploy key, workspace service startup, network firewall configuration); they differ only in stdio and per-launch configuration source.

Before launching the agent container, `wrix` ensures the per-workspace service container (`<repo>-service`) is running when beads or the project Nix cache is enabled. Dolt endpoints and project-cache `NIX_CONFIG` injection are owned by `services.md`; this spec owns only that both launcher subcommands use the same container construction path.

| Subcommand | Stdio | Configuration source | Use case |
|------------|-------|----------------------|----------|
| `wrix run [DIR] [CMD…]` | TTY (`-it`) | `ProfileConfig` JSON + host env + CLI args | Interactive sessions, `nix run .#sandbox-<profile>` |
| `wrix spawn --spawn-config <file> [--stdio]` | Piped or detached | `ProfileConfig` JSON + per-launch `SpawnConfig` JSON | Programmatic dispatch (loom; future orchestrators) |

`ProfileConfig` JSON is generated by Nix into the store and contains the immutable profile/image defaults. It is data, not shell code, and the Rust CLI validates it before constructing platform container argv. Schema v1:

```json
{
  "schema": 1,
  "system": "x86_64-linux",
  "profile": {
    "name": "rust",
    "env": { "KEY": "value" },
    "mounts": [
      { "source": "~/.cargo/registry", "dest": "/home/wrix/.cargo/registry", "mode": "rw" }
    ],
    "writable_dirs": ["/home/wrix/.cargo"],
    "network_allowlist": ["api.anthropic.com", "github.com", "cache.nixos.org"]
  },
  "image": {
    "ref": "localhost/wrix-rust:sha256-...",
    "source": "/nix/store/...-wrix-rust-oci.tar",
    "digest": "sha256:..."
  },
  "agent": {
    "kind": "direct"
  },
  "resources": {
    "cpus": null,
    "memory_mb": 4096,
    "pids_limit": 4096
  },
  "security": {
    "deploy_key": null
  },
  "network": {
    "default_mode": "open",
    "ipv6": "disabled"
  },
  "services": {
    "beads": { "enable": "auto" },
    "nix_cache": { "enable": true }
  },
  "features": {
    "mcp_runtime": false
  }
}
```

`profile.mounts` are profile-level mounts and are additive with `mkSandbox.mounts` and `SpawnConfig.mounts`. `profile.env` is profile/default container environment and is overridden only by explicit per-launch env rules. `agent.kind` is one of `direct`, `claude`, or `pi`; callers may not change it independently of `image`. `services.beads.enable = "auto"` means start Dolt when the workspace has `.beads/dolt`; `services.nix_cache.enable` controls project-cache endpoint injection. `network.default_mode` defaults to `open` and may be overridden at launch by `WRIX_NETWORK=open|limit`; both modes keep the local-network isolation baseline.

`SpawnConfig` JSON has stable top-level fields:

- `image_ref` — optional per-launch image ref override; when absent, `ProfileConfig.image.ref` is used.
- `image_source` — optional per-launch Nix store path of the streamable/loadable image artifact; when absent, `ProfileConfig.image.source` is used. The launcher installs the selected image into the platform store before invoking the container CLI; preflight + transport semantics are documented in *Image install path* above.
- `workspace` — host path bind-mounted at `/workspace`
- `env` — allowlist of `[key, value]` pairs to pass through
- `agent_args` — argv tail passed to the agent binary
- `mounts` — optional `[{host_path, container_path, read_only}]` list; omitted or empty means no per-launch mounts. Additive to `profile.mounts` and `mkSandbox.mounts`.

Plus consumer-defined fields the entrypoint reads from inside the container. The schema is part of the wrix CLI contract — see `wrix spawn --help`. Per-launch `SpawnConfig` may override launch-time inputs (workspace, env allowlist, agent args, mounts, image ref/source for orchestrators), but it may not change the selected agent independently of the image/profile config. `wrix run` errors when no valid `ProfileConfig` is supplied; there is no implicit default image baked in.

## Platform Implementations

### Linux (Podman)

- Rootless Podman remains the default Linux runtime; krun is optional.
- The launcher grants temporary in-container `NET_ADMIN` for firewall setup on every launch, including `WRIX_NETWORK=open`, because baseline LAN/private/host-local/VPN blocking is always required. The entrypoint installs the in-sandbox firewall ruleset (`nftables` by default on Linux Podman), disables/blocks IPv6 for v1, verifies policy, uses `capsh` to drop `NET_ADMIN`, and only then execs the agent.
- Default boundary runs as rootless **container-root** (no `--userns=keep-id`), which maps to the invoking host user — the owner of the baked `/nix/store` — so store-mutating Nix ops succeed and `/workspace` files carry host UID/GID. claude refuses `--dangerously-skip-permissions` as root, so the launcher sets `IS_SANDBOX=1` (claude's own escape hatch) — **not** `LD_PRELOAD=/lib/libfakeuid.so`: that `getuid()→1000` spoof blanks claude's TUI on this boundary when the process is really root (wx-nsage). The microVM path keeps `--userns=keep-id` (krun maps host user→root inside the VM) and **does** use libfakeuid via `krun-init.sh` (krun-relay's PTY tolerates the spoof)
- `--pids-limit 4096` fork-bomb guard
- `WRIX_MICROVM=1` switches to `podman --runtime krun` when `/dev/kvm` exists

### macOS (Apple `container` CLI)

- Requires macOS 26+ and Apple Silicon
- Virtualization.framework microVM, always (no separate container-mode path)
- vmnet networking with the same always-on in-guest firewall policy: no inbound ports, public-internet outbound in `open`, allowlist-only outbound in `limit`, LAN/private/host-local/VPN/special ranges blocked in both modes, IPv6 disabled/blocked for v1. The Darwin host `pf` firewall is not part of the sandbox contract.
- VirtioFS workspace mount
- Mount classifier handles `profile.mounts` and `SpawnConfig.mounts` uniformly — directories staged + copied at launch, regular files copy-from-parent-dir, Unix-socket sources rejected at launch
- Entrypoint creates user matching host UID

## Success Criteria

- `mkSandbox` accepts the documented parameter set (`profile`, `cpus`, `memoryMb`, `deployKey`, `packages`, `mounts`, `env`, `mcp`, `mcpRuntime`, `agent`, `agentPkg`, `agentSettings`) and returns `{ package, image, launcher, profile }`
  [check](grep -nE 'profile \?|cpus \?|memoryMb \?|deployKey \?|packages \?|mounts \?|env \?|mcp \?|mcpRuntime \?|agent \?|agentPkg \?|agentSettings \?|inherit package image launcher' lib/sandbox/default.nix)
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
- In a fresh container built from a profile that ships `nix`, the runtime process (rootless container-root) runs `nix develop -c true`, a `nix build` of a flake target, and a store-mutating op against a baked root-owned path to completion (exit 0) with no `Operation not permitted` failure on a `/nix/store` path
  [system](bash tests/sandbox/nix-in-container.sh)
- The default container boundary does not `LD_PRELOAD` `libfakeuid` (its `getuid()→1000` spoof blanks claude's TUI when the process is really root — wx-nsage); instead it sets `IS_SANDBOX=1` so claude permits `--dangerously-skip-permissions` as root without spoofing. libfakeuid remains krun-only
  [check](grep -q 'IS_SANDBOX=1' lib/sandbox/linux/default.nix && ! grep -q 'LD_PRELOAD=/lib/libfakeuid.so' lib/sandbox/linux/default.nix)
- A freshly provisioned container — with no prior store surgery — passes `nix-store --verify --check-contents` with zero missing or dangling paths, so an additive `nix build` cannot fail with `No such file or directory` on a path the baked Nix DB registers as valid (the build-time guarantee is owned by `image-builder.md` § In-Container Nix Store Consistency)
  [system](bash tests/sandbox/nix-store-verify-clean.sh)
- The launcher accepts `WRIX_NETWORK=open` and `WRIX_NETWORK=limit`; any other value errors before the container starts
  [check?](grep -nE "WRIX_NETWORK must be 'open' or 'limit'" lib/sandbox/linux/default.nix lib/sandbox/darwin/default.nix)
- In `WRIX_NETWORK=open`, sandbox outbound to public internet succeeds, but outbound to LAN/private/host-local/VPN/special IPv4 ranges fails except for exact DNS and wrix-owned endpoint exceptions
  [system?](bash tests/sandbox/network-baseline.sh test_open_blocks_lan)
- In `WRIX_NETWORK=limit`, outbound succeeds only to the merged allowlist plus exact DNS and wrix-owned endpoint exceptions; allowlist domains are resolved once at startup, unresolvable domains fail launch, and non-allowlisted public internet plus LAN/private/host-local/VPN/special ranges fail
  [system?](bash tests/sandbox/network-baseline.sh test_limit_allowlist)
- IPv6 egress is disabled or blocked in both network modes for v1
  [system?](bash tests/sandbox/network-baseline.sh test_ipv6_blocked)
- If firewall setup, IPv6 disablement, or `NET_ADMIN` drop cannot be verified, the launcher fails closed before the agent starts and never falls back to LAN-open networking
  [system?](bash tests/sandbox/network-baseline.sh test_fail_closed)
- After startup, the agent process cannot modify firewall rules (for example `nft flush ruleset` on the nft backend, or the equivalent backend flush command, fails inside the running sandbox)
  [system?](bash tests/sandbox/network-baseline.sh test_agent_lacks_net_admin)
- `WRIX_MICROVM=1` selects `podman --runtime krun` on Linux when `/dev/kvm` is available, and fails loudly when KVM is missing
  [check](grep -nE 'WRIX_MICROVM|--runtime krun|/dev/kvm' lib/sandbox/linux/default.nix)
- `wrix run` errors at startup with a clear message when no valid Nix-generated `ProfileConfig` JSON is supplied
  [system?](bash tests/sandbox/missing-profile-config.sh)
- `mkSandbox`'s `package` wrapper passes an immutable Nix-store `ProfileConfig` JSON path to the Rust `wrix` CLI instead of generating a large shell launcher
  [system?](bash tests/sandbox/profile-config-wrapper.sh)
- The selected agent runtime comes from `ProfileConfig` and cannot be changed by caller env independently of the selected image/profile
  [system?](bash tests/sandbox/profile-config-agent-pin.sh)
- `wrix spawn --spawn-config <file>` parses the documented `SpawnConfig` fields (`image_ref`, `image_source`, `workspace`, `env`, `agent_args`, `mounts`) and rejects attempts to change the selected agent independently of `ProfileConfig`
  [system?](bash tests/sandbox/spawn-config-schema.sh)
- On Linux, each `SpawnConfig.mounts` entry becomes a `-v <host_path>:<container_path>` podman argument, with `:ro` appended when `read_only: true`. A missing or empty `mounts` list produces no additional `-v` flags.
  [system?](bash tests/sandbox/spawn-config-mounts.sh)
- On Darwin, the same mount classifier handles `profile.mounts` and `SpawnConfig.mounts` — one mechanism, not two. Directories are staged + copied at launch, regular files copy-from-parent-dir, and entries whose `host_path` is a Unix socket cause the launcher to fail loudly before the container starts. (VirtioFS does not pass socket operations, so a silently-mounted socket would dead-end at the first `connect()`.)
  [system](bash tests/sandbox/darwin-mount-classifier.sh)
- The container entrypoint switches on `WRIX_AGENT` and exec's the matching agent binary (`claude`, `pi`, `direct`)
  [check](grep -nE 'WRIX_AGENT' lib/sandbox/linux/entrypoint.sh lib/sandbox/darwin/entrypoint.sh)
- Before exec'ing the selected agent, the entrypoint verifies the agent's binary is present and fails loudly with a clear error when it is absent from the image (e.g. `WRIX_AGENT=pi` against a claude image), rather than emitting a bare `command not found`
  [system](bash tests/sandbox/agent-binary-guard.sh)
- Both entrypoints seed and persist each agent's own config home — claude `~/.claude`, pi `~/.pi/agent` — not only claude's
  [check](grep -nE '\.pi/agent' lib/sandbox/linux/entrypoint.sh lib/sandbox/darwin/entrypoint.sh)
- Deploy key `<name>` is mounted at `/etc/wrix/keys/<name>` inside the container when `deployKey = "<name>"` is set (the `.pub` file is not mounted; the entrypoint regenerates it on demand via `ssh-keygen -y`)
  [check](grep -nE 'containerKeyDir|deployKey' lib/sandbox/linux/default.nix lib/sandbox/darwin/default.nix)
- `agentSettings` merges into the selected agent's baked settings; non-empty `agentSettings` with `agent = "direct"` fails at evaluation time
  [check](grep -nE 'agentSettings|baseClaudeSettings|basePiSettings' lib/sandbox/default.nix)
- When `/workspace/bin` exists inside the container, it appears first on `PATH`, so a consumer-supplied shim at `/workspace/bin/<name>` resolves ahead of a same-named binary baked into the image
  [system](bash tests/sandbox/workspace-bin-path.sh)
- When `/workspace/bin` does not exist, the container's `PATH` does not contain `/workspace/bin`
  [system](bash tests/sandbox/workspace-bin-path.sh)
- Both `lib/sandbox/linux/entrypoint.sh` and `lib/sandbox/darwin/entrypoint.sh` implement the `/workspace/bin` PATH prepend
  [check](grep -nE 'PATH="/workspace/bin:' lib/sandbox/linux/entrypoint.sh lib/sandbox/darwin/entrypoint.sh)
- The launcher preflight checks whether the image's content digest matches any image already present in the platform store before invoking the install pipeline; on a digest hit, no tar bytes are streamed and no `*-load` CLI is invoked
  [system](bash tests/sandbox/image-install-digest-skip.sh)
- On Linux, the launcher uses `skopeo copy oci-archive: → containers-storage:` for image install; the existing `podman load` call site is replaced
  [check](grep -nE 'skopeo.*containers-storage' lib/sandbox/linux/default.nix)
- A second spawn of an already-loaded image performs no writes to the platform store's layer directory (measurable via store size or per-blob mtime)
  [system?](bash tests/sandbox/image-install-no-rewrite.sh)
- On Linux, re-installing an image that differs from the cached one in only its top-of-closure layers transfers O(changed-blobs) bytes into the platform store, not O(image-size) bytes
  [system](bash tests/sandbox/image-install-delta-bounded.sh)
- On Darwin, the launcher uses `container image load --input <tar>` for image install (per Apple's available CLI surface) and relies on the digest-skip preflight and `wrix-base-image` chaining to bound install I/O
  [check](grep -nE 'container image load' lib/sandbox/darwin/default.nix)

## Requirements

### Functional

1. **mkSandbox API** — accepts the parameters above; returns `{ package, image, launcher, profile }`. Profile schema lives in `profiles.md`; image build in `image-builder.md`; MCP server contracts in `tmux-mcp.md` and `playwright-mcp.md`.
2. **Platform dispatch** — Linux selects the Podman launcher; macOS selects the Apple `container` CLI launcher; unsupported systems throw.
3. **Workspace mount** — CWD bind-mounts at `/workspace`; profile mounts merge on top.
4. **UID mapping** — files created in `/workspace` carry host UID/GID.
5. **Custom mounts and env** — `mkSandbox`'s `mounts` and `env` extend the profile rather than replace it.
6. **Deploy keys** — `deployKey = "<name>"` mounts the host key into the container at `/etc/wrix/keys/<name>` (and `/etc/wrix/keys/<name>-signing` when a signing key is present). The `.pub` file is not mounted; the entrypoint regenerates it on demand via `ssh-keygen -y`. Host-source resolution and the env-first override (`WRIX_DEPLOY_KEY`, `WRIX_SIGNING_KEY`) are owned by `security.md`.
7. **MCP opt-in** — `mcp.<server>` enables a named server per `tmux-mcp.md` / `playwright-mcp.md`. `mcpRuntime = true` bakes all registered servers and defers selection to the entrypoint.
8. **Agent runtime axis** — `agent` selects, at build time, the single agent binary baked into the image and launched by the entrypoint; exactly one agent per image (a non-claude image carries no `claude-code`). Selection is encoded in immutable `ProfileConfig`, not caller env. `WRIX_AGENT` remains only the launcher→entrypoint wire derived from that config. The entrypoint guards on binary presence (`command -v`) and seeds/persists each agent's own config home (claude `~/.claude`, pi `~/.pi/agent`). Agent selection adds only that agent's required config: Claude images get Claude settings, Pi images get non-secret Pi settings (`openai-codex`, `gpt-5.5`, high reasoning, explicit `/workspace/.pi/agent/sessions` session dir) plus a runtime `auth.json` mount when selected, and direct images get no agent config. `agentPkg` overrides the selected agent package; `agentSettings` merges into the selected agent's settings schema and is rejected for direct. Pi does not import arbitrary files from `/workspace/.pi/agent`; only the session directory and auth mount are wired. Secrets are delivered through env passthrough, credential-file mounts, and `agent_args` (owned by `security.md`). The agent runtime is its own image tier (`image-builder.md`), composing orthogonally with the profile.
9. **Launcher contract** — `wrix run` reads immutable Nix-generated `ProfileConfig` JSON plus CLI/host-env runtime inputs; `wrix spawn` reads the same `ProfileConfig` plus per-launch `SpawnConfig` JSON. Both share container construction, including workspace service startup and endpoint injection when services are enabled. Wrapper config-generation rules are owned by *Architecture > `package`*; workspace service contracts are owned by `services.md`.
10. **Per-launch mounts via SpawnConfig** — `wrix spawn`'s `SpawnConfig.mounts` adds per-launch bind mounts on top of `profile.mounts` and `mkSandbox`'s `mounts`. Each entry maps `host_path → container_path` with `read_only: true` rendering `:ro`. On Linux this is a literal `-v` flag. On Darwin, `SpawnConfig.mounts` flows through the same mount classifier as `profile.mounts`: directories staged + copied, regular files copy-from-parent-dir, Unix-socket sources rejected at launch with a clear error (VirtioFS does not pass socket operations). The launcher does not validate that `host_path` exists; podman fails at runtime if it does not.
11. **Workspace `bin/` PATH prepend** — When `/workspace/bin` exists inside the container, both Linux and macOS entrypoints prepend it to `PATH` so consumer-supplied shims under the workspace's `bin/` resolve ahead of image-baked binaries with the same name. The check is directory existence, not per-binary; the consumer owns what it ships in `bin/`. When the directory is absent, `PATH` is unchanged. The contract is PATH ordering only — wrix does not create `/workspace/bin`, does not validate its contents, and does not allowlist individual shims.
12. **In-container Nix** — a sandbox built from a `nix`-shipping profile lets the runtime user run both additive (`nix develop`, `nix build` of new closures) and store-mutating (replace, GC, delete of baked paths) Nix operations without permission or missing-path failures. On the default boundary the runtime process is rootless container-root, which maps to the host user that owns the baked store, so it can mutate root-owned store paths — the `deletePath → fchmodat2(u+w)` primitive no longer hits `EPERM`. Independently, correctness against a missing-path failure (`No such file or directory` on a registered path) relies on the image shipping a Nix database that exactly matches its on-disk store — no orphaned (on-disk but unregistered) and no dangling (registered but absent) paths in either direction (mechanism owned by `image-builder.md` § In-Container Nix Store Consistency).

### Non-Functional

1. **Rootless / no elevated privileges** — Linux runs rootless Podman; macOS runs the Apple `container` CLI as the calling user. No host capabilities granted.
2. **Boundary class** — macOS is always microVM; Linux defaults to rootless container, opts into microVM with `WRIX_MICROVM=1` (see `specs/security.md`).
3. **Network posture** — no inbound ports on either platform. In both `WRIX_NETWORK=open` and `WRIX_NETWORK=limit`, LAN/private/host-local/VPN/special outbound is blocked with exact DNS and wrix-owned endpoint exceptions only. `open` allows public-internet outbound; `limit` restricts public egress to the merged allowlist. Filtering is fail-closed and drops `NET_ADMIN` before the agent starts.
4. **Near-native performance** — minimal overhead beyond the container/microVM boundary cost; krun adds ~100MB per microVM.

## Out of Scope

- Windows support
- GPU passthrough
- Inbound port forwarding
- User-defined unsafe networking modes; wrix does not provide a LAN-open escape hatch
- Per-user multi-tenant sharing (sandboxes are single-user-per-host by design)
