# Core Sandbox

Platform-agnostic container isolation for coding agents — composes a workspace profile, an OCI image source, and a launcher binary; runs on Linux (Podman, optionally krun-backed microVM) and macOS (Apple `container` CLI on Virtualization.framework).

## Problem Statement

Running AI coding assistants with unrestricted host access creates security risks. The sandbox must protect host filesystem and processes from container actions, preserve host UID/GID for workspace files, support outbound network for research and package management, and work consistently across Linux and macOS without per-platform consumer code.

## Architecture

`mkSandbox` is the entry point. It composes three concerns owned elsewhere and returns a sandbox attrset:

- A workspace **profile** — packages, env, mounts, network allowlist, plugins (`profiles.md`)
- An OCI **image source** built from the profile and the selected agent runtime (`image-builder.md`)
- A profile-agnostic **launcher** binary (`wrix`; root command grammar owned by `cli.md`)

`mkSandbox` returns `{ package, image, launcher, profile, devShell }`:

- `package` — a configured sandbox package. `bin/wrix` is the explicit configured CLI, and the package `meta.mainProgram` is `wrix-run`, which defaults `nix run .#sandbox-*` to `wrix run`. The wrapper is bound to its built `(profile × agent)` image variant; agent selection and image defaults come from the JSON, not mutable shell logic. One-shot users invoke `package` directly.
- `image` — the per-profile OCI image-source derivation/attrset. It carries the source path plus metadata (`ref`, `source_kind`, `digest`, and `profileConfig`) so orchestrators can feed it through the platform install path without re-deriving tags or source-kind rules (see *Image install path* below).
- `launcher` — the raw Rust `wrix` derivation. Orchestrators (e.g. loom) pass `--profile-config <store-path>` and, for `spawn`, a per-launch `SpawnConfig` JSON.
- `profile` — the resolved profile attrset after merging consumer `packages`, `mounts`, `env`, and MCP server packages.
- `devShell` — a helper function for host devshells backed by this sandbox object, so `wrix run` inside the shell and `nix run .#sandbox-*` use the same configured package.

**Platform dispatch** — `lib/sandbox/default.nix` selects the platform image source, entrypoint, and configured wrapper metadata, then rejects unsupported systems at evaluation. The profile-agnostic Rust `wrix` launcher performs host runtime dispatch at execution time: Linux constructs the Podman invocation, and Darwin constructs the Apple `container` invocation.

The **runtime image installer** is the shared host-side image install and cleanup path used by `wrix run`, `wrix spawn`, and `wrix service start`; it is not a separate public CLI.

**Image install path** — Before invoking the platform install pipeline, the wrix runtime image installer checks whether the image's **content digest** recorded with the selected image source (not ref-name+tag) matches any image already present in the platform store. On Linux this digest is derived from descriptor/config metadata without executing the source; tar-loadable Darwin sources may be inspected for config metadata but are not loaded. On a digest hit, the install is skipped entirely — no source execution, no tar materialization, no stream invocation, and no `*-load` CLI call. On a miss, the installer dispatches by `ProfileConfig.image.source_kind`:

- Linux uses an archive-less source (`source_kind = "nix-descriptor"`) whose descriptor names a prebuilt OCI layout. The runtime installer reads that descriptor and copies `oci:<oci_layout>:<oci_ref>` into `containers-storage:<ref>` with skopeo (or an equivalent wrix-owned copy path). Digest preflight runs before the copy; on a miss, wrix delegates the copy to the destination store's content-addressed transport.
- Darwin converts tar-loadable sources (`source_kind = "docker-archive"`) to a temporary OCI archive, then invokes `container image load --input <oci-archive>`. Apple's `container` CLI surfaces no per-blob-dedup install path at this time; see `image-builder.md` § Out of Scope.

Both platforms rely on the provenance-tiered graph (see `image-builder.md` § Provenance-Tiered Layering) to keep volatile changes isolated. Linux realizes the cache contract through descriptor-level layer reuse; Darwin keeps a tar/load fallback plus digest-skip preflight until a per-blob Apple path is verified.

**Image retention and cleanup** — The wrix runtime image cleanup path maintains a bounded wrix image keep set across workspaces, stored under the user's wrix cache (implementation-owned path, file name `image-mru.json`) rather than under a single repo. It keeps the image selected for the current operation, images used by existing containers, and the eight most recently used wrix image records written by any workspace/direnv. Each MRU record includes the image ref plus the resolved content digest and image ID when available; cleanup keeps an image if any recorded identifier matches. Cleanup consults that shared MRU before deleting so a launch in one repo does not remove another repo's recently cached image. Wrix-managed images outside that keep set are pruned. New images are labelled by `image-builder.md` so dangling cleanup can target wrix-owned images without touching user images. On Darwin, a successful archive load is tagged with its stable wrix ref and its temporary Apple `untagged@sha256:<digest>` load ref is removed immediately. Retention also recognizes historical untagged records as cleanup candidates when their image variant carries `wrix.managed=true`. On Linux, legacy tagged `localhost/wrix-*` images may be removed when outside the keep set. Unlabelled dangling images are not automatically removed on either platform because ownership is ambiguous; Wrix may report those images and offer a manual/opt-in cleanup path.

**Boundary class** —

- macOS: microVM via Virtualization.framework, always
- Linux: rootless container by default; `WRIX_MICROVM=1` opts into `podman --runtime krun` when `/dev/kvm` is available. krun is bundled via Nix; without KVM the opt-in fails loudly rather than silently degrading.

The host Podman API is outside the normal sandbox boundary. Linux exposes it only through the explicit unsafe operator opt-in `WRIX_UNSAFE_PODMAN_SOCKET`, which mounts the host user's Podman socket and exports `CONTAINER_HOST` for sibling-container workflows. The legacy `WRIX_PODMAN_SOCKET` name has no effect.

Threat-model rationale for these choices lives in `specs/security.md`.

**Network posture** — `WRIX_NETWORK` selects public egress posture at launch time. The launcher passes the mode, merged allowlist, DNS exceptions, and wrix-owned local endpoint exceptions into the container via env/config; the Linux entrypoint installs an in-sandbox firewall ruleset before the agent starts, while Darwin uses an immutable first-stage bootstrap before any workspace setup or agent code runs. Linux Podman uses `nftables` by default. Darwin does not use host `pf`; it uses the firewall backend available inside the Linux guest/container (`nftables` when supported, otherwise a verified equivalent such as iptables).

Baseline network isolation is always enforced in both modes: no inbound ports, IPv6 disabled/blocked for v1, and outbound traffic to LAN/private/host-local/VPN/special ranges is blocked. Exact exceptions are allowed only for wrix-owned endpoints (for example the project cache host-gateway IP/port, Darwin Dolt TCP endpoint) and configured DNS resolvers on TCP/UDP port 53.

- `open` (default) — public-internet outbound is allowed, but LAN/private/host-local/VPN/special ranges remain blocked.
- `limit` — outbound is restricted to the profile's merged `networkAllowlist` plus exact wrix-owned local endpoint and DNS exceptions; LAN/private/host-local/VPN/special ranges remain blocked. Any other value errors at the launcher before the container starts.

Filtering is fail-closed. Linux rootless Podman grants temporary in-container `NET_ADMIN` so the entrypoint can install namespace-local firewall rules atomically, then uses `capsh` to drop `NET_ADMIN` before execing the agent. `WRIX_MICROVM=1` remains an optional stronger boundary, not a requirement. macOS runs in a microVM unconditionally; its immutable bootstrap alone receives `NET_ADMIN`, uses only image-pinned tools, verifies the policy, and replaces itself through `capsh` with a stage that rejects `NET_ADMIN` in every Linux capability set before touching `/workspace`. The Darwin host firewall is never mutated. `WRIX_NETWORK=limit` domains are resolved once at startup; any unresolvable allowlist domain fails launch instead of being silently omitted. If firewall setup, IPv6 disablement, or capability drop cannot be verified, launch fails; wrix never falls back to LAN-open networking.

**Agent runtime axis** — the `agent` parameter selects, **at build time**, the single agent binary the image bakes and the entrypoint launches. The binary must be in the image, so this is not a runtime knob.

- `direct` (default) — default base image; consumers can override the placeholder with `agentPkg`
- `claude` — `claude-code` from nixpkgs; no consumer package required
- `pi` — `pi-coding-agent` from nixpkgs by default

Exactly one agent rides each image — `agent = "direct"` bakes neither `claude-code` nor `pi`. The agent runtime is its own image tier (`image-builder.md` § Provenance-Tiered Layering); it composes orthogonally with the profile, so variants are `(profile × agent)`.

**Selection is by build target, not by caller env.** `WRIX_AGENT` is the internal wire the entrypoint reads, but callers do not select it by exporting env vars. A human selects an agent by choosing the `mkSandbox { agent = …; }` build / its `sandbox-<profile>[-<agent>]` target; that choice is encoded in the immutable `ProfileConfig` JSON. Orchestrators driving the raw `launcher` pass a matching per-call `ProfileConfig`.

**Entrypoint agent guards.** The image declares its baked agent variant in `/etc/wrix/image-agent`. The entrypoint dispatches on `WRIX_AGENT` and, before exec, first rejects a mismatch between the ProfileConfig-selected agent and the image-declared agent with a clear ProfileConfig/image-variant error, then verifies the named binary is present (`command -v`). A request for an agent absent from the image — e.g. `WRIX_AGENT=pi` against a claude image on the raw-launcher path — fails loudly with a clear error instead of a bare `command not found`.

**Per-agent configuration is delivered, not abstracted.** Each agent keeps its own config system; wrix only delivers config to it:

- *Config home* — the entrypoint seeds the agent's config home from baked defaults and persists session data via `/workspace`, per agent: claude → `~/.claude`, pi → `~/.pi/agent`; `direct` has none.
- *Credentials* — API keys and OAuth/subscription tokens reach the agent via ordinary env passthrough (`env`, host env, `SpawnConfig.env`) and mounts; secrets are never baked into the image. The credential invariants are owned by `security.md`.
- *Package/settings overrides* — `agentPkg` overrides the selected agent package; `agentSettings` merges into the selected agent's settings schema. `agentSettings` is rejected for `agent = "direct"` until direct has a settings schema.

**MCP servers** — `mkSandbox`'s `mcp` parameter opts servers in per sandbox (`mcp.tmux = { … }`, `mcp.playwright = { … }`). Server contracts live in their own specs (`tmux-mcp.md`, `playwright-mcp.md`). `mcpRuntime = true` is an all-server runtime-selection axis: it bakes every registered server into the image and defers selection to the entrypoint at run time. Profile output names for the runtime bundle live in `profiles.md`.

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

Returns `{ package, image, launcher, profile, devShell }`.

## Launcher Runtime Contract

`cli.md` owns that the launcher is exposed as `wrix run` and `wrix spawn`, including help and root dispatch. This section owns the runtime contract behind those subcommands. The Rust `wrix` launcher binary is profile-agnostic. Nix supplies build-time defaults through an immutable `ProfileConfig` JSON file, passed by `--profile-config <path>` or a wrapper-set equivalent. Both launcher subcommands share container construction (mounts, env passthrough, runtime selection, deploy key, workspace service startup, network firewall configuration); they differ only in stdio and per-launch configuration source.

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
    "name": "example",
    "env": { "KEY": "value" },
    "mounts": [
      { "source": "~/.config/example", "dest": "/home/wrix/.config/example", "mode": "ro", "optional": false }
    ],
    "writable_dirs": [
      "/home/wrix/.cargo",
      "/home/wrix/.cache"
    ],
    "network_allowlist": ["example.org"]
  },
  "image": {
    "ref": "localhost/wrix-example:sha256-...",
    "source": "/nix/store/...-wrix-example-image.json",
    "source_kind": "nix-descriptor",
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
- `image_source` — optional per-launch Nix store path of the image source; when absent, `ProfileConfig.image.source` is used.
- `image_source_kind` — optional per-launch source-kind override (`nix-descriptor` or `docker-archive`); when absent, `ProfileConfig.image.source_kind` is used. If `image_source` is present, `image_source_kind` must also be present, even when it matches the profile kind, so source overrides never rely on launcher inference. For an `image_source` override, the installer derives and validates the selected image digest from that override source before preflight instead of reusing `ProfileConfig.image.digest`. The installer installs the selected image into the platform store before the launcher invokes the container CLI; preflight + dispatch semantics are documented in *Image install path* above.
- `workspace` — host path bind-mounted at `/workspace`
- `env` — allowlist of `[key, value]` pairs to pass through
- `agent_args` — argv tail passed to the agent binary
- `mounts` — optional `[{host_path, container_path, read_only}]` list; omitted or empty means no per-launch mounts. Additive to `profile.mounts` and `mkSandbox.mounts`.

Plus consumer-defined fields the entrypoint reads from the original config mounted read-only inside the container at the path named by `WRIX_SPAWN_CONFIG`. The schema is part of the launcher runtime contract, and CLI help mirrors it per `cli.md`. Per-launch `SpawnConfig` may override launch-time inputs (workspace, env allowlist, agent args, mounts, image ref/source for orchestrators), but it may not change the selected agent independently of the image/profile config. `wrix run` errors when no valid `ProfileConfig` is supplied; there is no implicit default image baked in.

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
- An immutable `/network-bootstrap.sh` is the only stage granted `NET_ADMIN`; it uses image-pinned binaries, verifies the firewall, and `exec`s `/entrypoint.sh` through `capsh` after dropping `NET_ADMIN`. The agent entrypoint fails before workspace setup unless the trusted bootstrap marker exists and `NET_ADMIN` is absent from inheritable, permitted, effective, bounding, and ambient capability sets.
- VirtioFS workspace mount
- Mount classifier handles `profile.mounts` and `SpawnConfig.mounts` uniformly — directories staged + copied at launch, regular files copy-from-parent-dir, Unix-socket sources rejected at launch
- Entrypoint creates user matching host UID

## Success Criteria

- `mkSandbox` accepts the documented parameter set (`profile`, `cpus`, `memoryMb`, `deployKey`, `packages`, `mounts`, `env`, `mcp`, `mcpRuntime`, `agent`, `agentPkg`, `agentSettings`) and returns `{ package, image, launcher, profile, devShell }`
  [check](verify:sandbox.mksandbox-api)
- Platform dispatch picks the Linux implementation on Linux hosts and the macOS implementation on Darwin hosts
  [check](verify:sandbox.platform-dispatch)
- Evaluating `mkSandbox` on an unsupported system throws at evaluation time rather than producing a broken derivation
  [check](verify:sandbox.unsupported-system-error)
- A built Linux sandbox starts a container and exits cleanly
  [system](verify:sandbox.linux-container-starts)
- A built macOS sandbox starts an Apple `container` microVM and exits cleanly
  [system](verify:sandbox.darwin-container-starts)
- The Darwin network bootstrap cannot resolve tools from `/workspace`, verifies the firewall before invoking `capsh`, preserves the agent argv, and enters stage two only after requesting an irreversible `NET_ADMIN` drop
  [system](verify:sandbox.darwin-network-bootstrap)
- Files created inside `/workspace` carry the host UID/GID, not a container-internal UID
  [system](verify:sandbox.uid-mapping)
- Host filesystem outside `/workspace` and declared mounts is not visible inside the container
  [system](verify:sandbox.filesystem-isolation)
- `mounts` and `env` passed to `mkSandbox` are merged into the profile and reach the container as configured
  [system](verify:sandbox.custom-mounts-env)
- Every sandbox image carries the `wrix` CLI, so `wrix beads push` resolves inside the container without entering `nix develop` or running `nix run`
  [check](test-ci:test-wrix-cli-in-profile)
- In a fresh container built from a profile that ships `nix`, the runtime process (rootless container-root) runs `nix develop -c true`, a `nix build` of a flake target, and a store-mutating op against a baked root-owned path to completion (exit 0) with no `Operation not permitted` failure on a `/nix/store` path
  [system](verify:sandbox.nix-in-container)
- The default container boundary does not `LD_PRELOAD` `libfakeuid` (its `getuid()→1000` spoof blanks claude's TUI when the process is really root — wx-nsage); instead it sets `IS_SANDBOX=1` so claude permits `--dangerously-skip-permissions` as root without spoofing. libfakeuid remains krun-only
  [test](../crates/wrix-sandbox/tests/launch.rs::linux_default_boundary_sets_is_sandbox_without_fakeuid)
- A freshly provisioned container — with no prior store surgery — passes `nix-store --verify --check-contents` with zero missing or dangling paths, so an additive `nix build` cannot fail with `No such file or directory` on a path the baked Nix DB registers as valid (the build-time guarantee is owned by `image-builder.md` § In-Container Nix Store Consistency)
  [system](verify:sandbox.nix-store-verify-clean)
- The launcher accepts `WRIX_NETWORK=open` and `WRIX_NETWORK=limit`; any other value errors before the container starts
  [test](command::launch::test::network_mode_parse_accepts_only_open_and_limit)
- In `WRIX_NETWORK=open`, sandbox outbound to public internet succeeds, but outbound to LAN/private/host-local/VPN/special IPv4 ranges fails except for exact DNS and wrix-owned endpoint exceptions
  [system](verify:sandbox.network-open-blocks-lan)
- In `WRIX_NETWORK=limit`, outbound succeeds only to the merged allowlist plus exact DNS and wrix-owned endpoint exceptions; allowlist domains are resolved once at startup, unresolvable domains fail launch, and non-allowlisted public internet plus LAN/private/host-local/VPN/special ranges fail
  [system](verify:sandbox.network-limit-allowlist)
- IPv6 egress is disabled or blocked in both network modes for v1
  [system](verify:sandbox.network-ipv6-blocked)
- If firewall setup, IPv6 disablement, or `NET_ADMIN` drop cannot be verified, the launcher fails closed before the agent starts and never falls back to LAN-open networking
  [system](verify:sandbox.network-fail-closed)
- After startup, the agent process cannot modify firewall rules (for example `nft flush ruleset` on the nft backend, or the equivalent backend flush command, fails inside the running sandbox)
  [system](verify:sandbox.agent-lacks-net-admin)
- `WRIX_MICROVM=1` selects `podman --runtime krun --userns=keep-id` on Linux when `/dev/kvm` is available, enters through `/krun-relay`, serializes the requested command through `WRIX_KRUN_CMD`, passes terminal dimensions for PTY setup, reaches the krun-only `krun-init.sh`/libfakeuid boundary, and fails loudly when KVM is missing
  [system](verify:sandbox.linux-microvm-runtime)
- `wrix run` errors at startup with a clear message when no valid Nix-generated `ProfileConfig` JSON is supplied
  [test](../crates/wrix-sandbox/tests/command.rs::run_requires_valid_profile_config)
- `mkSandbox`'s `package` wrapper keeps `bin/wrix` explicit, exposes `wrix-run` as `meta.mainProgram` for `nix run`, and passes an immutable Nix-store `ProfileConfig` JSON path to the profile-agnostic launcher for both `run` and `spawn`, with image defaults supplied by `ProfileConfig` rather than mutable `WRIX_DEFAULT_IMAGE_*` env vars
  [check](test-ci:test-profile-config-wrapper)
- `ProfileConfig.image` includes `ref`, `source`, explicit `source_kind`, and `digest`; the launcher/runtime installer rejects configs where `source_kind` is missing or incompatible with the selected platform install path
  [check](test-ci:test-profile-config-image-source-kind)
- The selected agent runtime comes from `ProfileConfig` and cannot be changed by caller env independently of the selected image/profile
  [test](../crates/wrix-sandbox/tests/command.rs::profile_config_agent_cannot_be_overridden_by_env)
- `wrix spawn --spawn-config <file>` parses the documented `SpawnConfig` fields (`image_ref`, `image_source`, `image_source_kind`, `workspace`, `env`, `agent_args`, `mounts`) into the launch plan
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::documented_spawn_config_fields_render_into_launch_plan)
- A `SpawnConfig.image_source` override requires an explicit source kind compatible with the current platform
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::image_source_override_requires_source_kind)
- `SpawnConfig` cannot change the selected agent independently of `ProfileConfig`
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::spawn_config_cannot_override_agent)
- Consumer-defined `SpawnConfig` fields remain available to the in-container entrypoint through the read-only config mount and `WRIX_SPAWN_CONFIG`
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::consumer_spawn_config_fields_are_mounted_for_entrypoint)
- On Linux, each `SpawnConfig.mounts` entry becomes a `-v <host_path>:<container_path>` podman argument, with `:ro` appended when `read_only: true`. A missing or empty `mounts` list produces no additional `-v` flags.
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::linux_spawn_mounts_render_podman_volume_args)
- The packaged launcher does not mount the host Podman socket or export `CONTAINER_HOST` / `GC_HOST_*` by default; `WRIX_PODMAN_SOCKET` is ignored; and only `WRIX_UNSAFE_PODMAN_SOCKET` renders a real socket mount plus host-visible `CONTAINER_HOST` / `GC_HOST_*`, failing loudly when the socket is absent
  [system](verify:sandbox.unsafe-podman-socket)
- On Darwin, the same mount classifier handles `profile.mounts` and `SpawnConfig.mounts` — one mechanism, not two. Directories are staged + copied at launch, regular files copy-from-parent-dir, and entries whose `host_path` is a Unix socket cause the launcher to fail loudly before the container starts. (VirtioFS does not pass socket operations, so a silently-mounted socket would dead-end at the first `connect()`.)
  [test](../crates/wrix-sandbox/tests/darwin_mounts.rs::mount_classifier_handles_profile_and_spawn_mounts_uniformly)
- The container entrypoint switches on `WRIX_AGENT` and exec's the matching agent binary (`claude`, `pi`, `direct`)
  [check](verify:sandbox.entrypoint-agent-dispatch)
- Before exec'ing the selected agent, the entrypoint rejects a mismatch between the ProfileConfig-selected `WRIX_AGENT` and the image-declared `/etc/wrix/image-agent`, then verifies the agent's binary is present and fails loudly with a clear error when it is absent from the image (e.g. `WRIX_AGENT=pi` against a claude image), rather than emitting a bare `command not found`
  [system](verify:sandbox.agent-binary-guard)
- Both entrypoints seed and persist each agent's own config home — claude `~/.claude`, pi `~/.pi/agent` — not only claude's
  [check](verify:sandbox.agent-config-homes)
- Deploy key `<name>` is mounted at `/etc/wrix/keys/<name>` inside the container when `deployKey = "<name>"` is set (the `.pub` file is not mounted; the entrypoint regenerates it on demand via `ssh-keygen -y`)
  [test](../crates/wrix-sandbox/tests/launch.rs::deploy_key_mount_uses_container_key_dir_without_public_key)
- `agentSettings` merges into the selected agent's baked settings; non-empty `agentSettings` with `agent = "direct"` fails at evaluation time
  [check](test-ci:test-sandbox-agent-settings)
- Pi images seed `editorPaddingX = 1` and `enableInstallTelemetry = false`, so the input editor has one cell of horizontal padding and Pi's anonymous install/update ping plus optional provider attribution headers are disabled by default; update checking remains a separate Pi setting.
  [check](test-ci:test-sandbox-agent-settings)
- When `/workspace/bin` exists inside the container, it appears first on `PATH`, so a consumer-supplied shim at `/workspace/bin/<name>` resolves ahead of a same-named binary baked into the image
  [system](verify:sandbox.workspace-bin-path-present)
- When `/workspace/bin` does not exist, the container's `PATH` does not contain `/workspace/bin`
  [system](verify:sandbox.workspace-bin-path-absent)
- Both `lib/sandbox/linux/entrypoint.sh` and `lib/sandbox/darwin/entrypoint.sh` implement the `/workspace/bin` PATH prepend
  [check](verify:sandbox.entrypoint-workspace-bin-prepend)
- The packaged runtime image installer preflight checks whether the selected image source's content digest matches any image already present in the platform store before invoking the install pipeline; on a digest hit, no image source is executed, no tar bytes are streamed, and no `*-load` CLI is invoked
  [system](test-ci:test-image-install-digest-skip)
- On Linux, the runtime image installer dispatches `source_kind = "nix-descriptor"` through an archive-less descriptor-to-OCI-layout install path (`oci:<oci_layout>:<oci_ref>` → `containers-storage:<ref>` with skopeo, or equivalent wrix); the docker/OCI archive conversion path is not used for Linux descriptor sources
  [test](../crates/wrix-sandbox/tests/image_install.rs::linux_descriptor_sources_use_archiveless_install_path)
- A second spawn of an already-loaded image performs no writes to the platform store's layer directory and does not execute the image source
  [test](../crates/wrix-sandbox/tests/image_install.rs::already_loaded_image_performs_no_store_writes)
- The runtime image cleanup path records a bounded cross-workspace MRU of eight typed wrix image refs/digests/image IDs, preserves images used by Podman containers, prunes wrix-managed images outside the keep set, and does not automatically remove unlabelled `<none>:<none>` images
  [test](../crates/wrix-sandbox/tests/image_retention.rs::cleanup_prunes_only_wrix_managed_images_outside_bounded_keep_set)
- Concurrent launches update the shared MRU without losing either workspace's record or exposing partially-written JSON
  [test](../crates/wrix-sandbox/tests/image_retention.rs::concurrent_mru_updates_preserve_each_workspace_record)
- Apple `container list` records are parsed for image references and descriptor IDs so cleanup preserves images used by existing Apple containers
  [test](image::test::apple_container_list_preserves_images_used_by_existing_containers)
- Runtime MCP selection and tmux audit overrides from the host reach the bundled entrypoint through `WRIX_MCP` and `WRIX_MCP_TMUX_*`
  [test](../crates/wrix-sandbox/tests/launch.rs::runtime_mcp_host_configuration_reaches_entrypoint)
- On Darwin, the runtime image installer converts `source_kind = "docker-archive"` sources to temporary OCI archives before invoking `container image load --input <oci-archive>`, then removes the temporary archive and relies on digest-skip preflight while per-blob install remains out of scope
  [system](verify:sandbox.darwin-image-load)

## Requirements

### Functional

1. **mkSandbox API** — accepts the parameters above; returns `{ package, image, launcher, profile, devShell }`. Profile schema lives in `profiles.md`; image build in `image-builder.md`; MCP server contracts in `tmux-mcp.md` and `playwright-mcp.md`.
2. **Platform dispatch** — Linux selects the Podman launcher; macOS selects the Apple `container` CLI launcher; unsupported systems throw.
3. **Workspace mount** — CWD bind-mounts at `/workspace`; profile mounts merge on top.
4. **UID mapping** — files created in `/workspace` carry host UID/GID.
5. **Custom mounts and env** — `mkSandbox`'s `mounts` and `env` extend the profile rather than replace it.
6. **Deploy keys** — `deployKey = "<name>"` mounts the host key into the container at `/etc/wrix/keys/<name>` (and `/etc/wrix/keys/<name>-signing` when a signing key is present). The `.pub` file is not mounted; the entrypoint regenerates it on demand via `ssh-keygen -y`. Host-source resolution and the env-first override (`WRIX_DEPLOY_KEY`, `WRIX_SIGNING_KEY`) are owned by `security.md`.
7. **MCP opt-in** — `mcp.<server>` enables a named server per `tmux-mcp.md` / `playwright-mcp.md`. `mcpRuntime = true` is the all-server runtime-selection path: it bakes every registered server and defers selection to the entrypoint while profile output naming remains in `profiles.md`.
8. **Agent runtime axis** — `agent` selects, at build time, the single agent binary baked into the image and launched by the entrypoint; exactly one agent per image (a non-claude image carries no `claude-code`). Selection is encoded in immutable `ProfileConfig`, not caller env. `WRIX_AGENT` remains only the launcher→entrypoint wire derived from that config. The entrypoint guards on binary presence (`command -v`) and seeds/persists each agent's own config home (claude `~/.claude`, pi `~/.pi/agent`). Agent selection adds only that agent's required config: Claude images get Claude settings, Pi images get non-secret Pi settings (`openai-codex`, `gpt-5.6-sol`, xhigh reasoning, `defaultProjectTrust = "always"`, `editorPaddingX = 1`, `enableInstallTelemetry = false`, steering/follow-up modes set to `"all"`, explicit `/workspace/.pi/agent/sessions` session dir) plus a runtime `auth.json` mount when selected, and direct images get no agent config. `agentPkg` overrides the selected agent package; `agentSettings` merges into the selected agent's settings schema and is rejected for direct. Pi does not import arbitrary files from `/workspace/.pi/agent`; only the session directory and auth mount are wired. Secrets are delivered through env passthrough, credential-file mounts, and `agent_args` (owned by `security.md`). The agent runtime is its own image tier (`image-builder.md`), composing orthogonally with the profile.
9. **Launcher contract** — `wrix run` reads immutable Nix-generated `ProfileConfig` JSON plus CLI/host-env runtime inputs; `wrix spawn` reads the same `ProfileConfig` plus per-launch `SpawnConfig` JSON. Both share container construction, including workspace service startup and endpoint injection when services are enabled. Wrapper config-generation rules are owned by *Architecture > `package`*; workspace service contracts are owned by `services.md`.
10. **Image source dispatch** — image install dispatches on explicit source kind, not filename or platform guessing. `nix-descriptor` sources are Linux archive-less descriptor sources and `docker-archive` sources are tar-loadable archive sources. A per-launch `image_source` override must carry a matching `image_source_kind`; it may not silently inherit an incompatible kind from `ProfileConfig`. The selected source's digest is derived or validated before preflight.
11. **Image retention** — runtime image cleanup is wrix-scoped and bounded: keep the image selected for the current operation, images used by existing containers, and eight shared cross-workspace MRU records (ref plus digest/image ID when available) before pruning; prune wrix-managed images outside the keep set; never automatically delete unlabelled `<none>:<none>` images.
12. **Per-launch mounts via SpawnConfig** — `wrix spawn`'s `SpawnConfig.mounts` adds per-launch bind mounts on top of `profile.mounts` and `mkSandbox`'s `mounts`. Each entry maps `host_path → container_path` with `read_only: true` rendering `:ro`. On Linux this is a literal `-v` flag. On Darwin, `SpawnConfig.mounts` flows through the same mount classifier as `profile.mounts`: directories staged + copied, regular files copy-from-parent-dir, Unix-socket sources rejected at launch with a clear error (VirtioFS does not pass socket operations). The launcher does not validate that `host_path` exists; podman fails at runtime if it does not.
13. **Workspace `bin/` PATH prepend** — When `/workspace/bin` exists inside the container, both Linux and macOS entrypoints prepend it to `PATH` so consumer-supplied shims under the workspace's `bin/` resolve ahead of image-baked binaries with the same name. The check is directory existence, not per-binary; the consumer owns what it ships in `bin/`. When the directory is absent, `PATH` is unchanged. The contract is PATH ordering only — wrix does not create `/workspace/bin`, does not validate its contents, and does not allowlist individual shims.
14. **In-container Nix** — a sandbox built from a `nix`-shipping profile lets the runtime user run both additive (`nix develop`, `nix build` of new closures) and store-mutating (replace, GC, delete of baked paths) Nix operations without permission or missing-path failures. On the default boundary the runtime process is rootless container-root, which maps to the host user that owns the baked store, so it can mutate root-owned store paths — the `deletePath → fchmodat2(u+w)` primitive no longer hits `EPERM`. Independently, correctness against a missing-path failure (`No such file or directory` on a registered path) relies on the image shipping a Nix database that exactly matches its on-disk store — no orphaned (on-disk but unregistered) and no dangling (registered but absent) paths in either direction (mechanism owned by `image-builder.md` § In-Container Nix Store Consistency).

### Non-Functional

1. **Rootless / no elevated privileges** — Linux runs rootless Podman; macOS runs the Apple `container` CLI as the calling user. No host capabilities are granted by default; `WRIX_UNSAFE_PODMAN_SOCKET` is an explicit unsafe opt-in outside the normal sandbox boundary.
2. **Boundary class** — macOS is always microVM; Linux defaults to rootless container, opts into microVM with `WRIX_MICROVM=1` (see `specs/security.md`).
3. **Network posture** — no inbound ports on either platform. In both `WRIX_NETWORK=open` and `WRIX_NETWORK=limit`, LAN/private/host-local/VPN/special outbound is blocked with exact DNS and wrix-owned endpoint exceptions only. `open` allows public-internet outbound; `limit` restricts public egress to the merged allowlist. Filtering is fail-closed and drops `NET_ADMIN` before the agent starts.
4. **Near-native performance** — minimal overhead beyond the container/microVM boundary cost; krun adds ~100MB per microVM.

## Out of Scope

- Windows support
- GPU passthrough
- Inbound port forwarding
- User-defined unsafe networking modes; wrix does not provide a LAN-open escape hatch
- Per-user multi-tenant sharing (sandboxes are single-user-per-host by design)
