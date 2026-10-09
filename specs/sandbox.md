# Core Sandbox

Platform-agnostic container isolation for coding agents — composes a workspace
profile, an OCI image source, and a launcher binary; runs on Linux (Podman,
optionally krun-backed microVM) and macOS (Apple `container` CLI on
Virtualization.framework).

## Problem Statement

Running AI coding assistants with unrestricted host access creates security
risks. The sandbox must protect host filesystem and processes from container
actions, preserve host UID/GID for workspace files, support outbound network for
research and package management, and work consistently across Linux and macOS
without per-platform consumer code.

## Architecture

`mkSandbox` is the entry point. It composes three concerns owned elsewhere and
returns a sandbox attrset:

- A workspace **profile** — packages, env, mounts, network allowlist, plugins
  (`profiles.md`)
- An OCI **image source** built from the profile and the selected agent runtime
  (`image-builder.md`)
- A profile-agnostic **launcher** binary (`wrix`; root command grammar owned by
  `cli.md`)

### Sandbox Outputs

`mkSandbox` returns `{ package, image, launcher, profile, devShell }`:

- `package` — a configured sandbox package. `bin/wrix` is the explicit
  configured CLI, and the package `meta.mainProgram` is `wrix-run`, which
  defaults `nix run .#sandbox-*` to `wrix run`. The wrapper is bound to its
  built `(profile × agent)` image variant; agent selection and image defaults
  come from the JSON, not mutable shell logic. One-shot users invoke `package`
  directly.
- `image` — the per-profile OCI image-source derivation/attrset. It carries the
  source path plus metadata (`ref`, `source_kind`, `digest`, and
  `profileConfig`) so orchestrators can feed it through the platform install
  path without re-deriving tags or source-kind rules (see _Image install path_
  below).
- `launcher` — the raw Rust `wrix` derivation. Orchestrators (e.g. loom) pass
  `--profile-config <store-path>` and, for `spawn`, a per-launch `SpawnConfig`
  JSON.
- `profile` — the resolved profile attrset after merging consumer `packages`,
  `mounts`, `env`, and MCP server packages.
- `devShell` — a helper function for host devshells backed by this sandbox
  object, so `wrix run` inside the shell and `nix run .#sandbox-*` use the same
  configured package.

### Platform Dispatch

`lib/sandbox/default.nix` selects the platform image source, entrypoint, and
configured wrapper metadata, then rejects unsupported systems at evaluation. The
profile-agnostic Rust `wrix` launcher performs host runtime dispatch at
execution time: Linux constructs the Podman invocation, and Darwin constructs
the Apple `container` invocation.

The **runtime image installer** is the shared host-side image install and
cleanup path used by `wrix run`, `wrix spawn`, and `wrix service start`; it is
not a separate public CLI.

### Image Install Path

Before invoking the platform install pipeline, the wrix runtime image installer
checks whether the image's **content digest** recorded with the selected image
source (not ref-name+tag) matches any image already present in the platform
store. On Linux this digest is derived from descriptor/config metadata without
executing the source; tar-loadable Darwin sources may be inspected for config
metadata but are not loaded. On a digest hit, the install is skipped entirely —
no source execution, no tar materialization, no stream invocation, and no
`*-load` CLI call. On a miss, the installer dispatches by
`ProfileConfig.image.source_kind` according to the stable source-kind contract
owned by `image-builder.md`:

- The Linux descriptor source names a prebuilt OCI layout. The runtime installer
  reads that descriptor and copies `oci:<oci_layout>:<oci_ref>` into
  `containers-storage:<ref>` with skopeo (or an equivalent wrix-owned copy
  path). Digest preflight runs before the copy; on a miss, wrix delegates the
  copy to the destination store's content-addressed transport.
- The Darwin tar-loadable source is converted to a temporary OCI archive before
  `container image load --input <oci-archive>`. Apple's `container` CLI surfaces
  no per-blob-dedup install path at this time; see `image-builder.md` § Out of
  Scope.

Both platforms rely on the provenance-tiered graph (see `image-builder.md` §
Provenance-Tiered Layering) to keep volatile changes isolated. Linux realizes
the cache contract through descriptor-level layer reuse; Darwin keeps a tar/load
fallback plus digest-skip preflight until a per-blob Apple path is verified.

### Image Retention and Cleanup

The wrix runtime image cleanup path maintains a bounded wrix image keep set
across workspaces, stored under the user's wrix cache (implementation-owned
path, file name `image-mru.json`) rather than under a single repo. It keeps the
image selected for the current operation, images used by existing containers,
and the eight most recently used wrix image records written by any
workspace/direnv. Each MRU record includes the image ref plus the resolved
content digest and image ID when available; cleanup keeps an image if any
recorded identifier matches. Cleanup consults that shared MRU before deleting so
a launch in one repo does not remove another repo's recently cached image.
Wrix-managed images outside that keep set are pruned. New images are labelled by
`image-builder.md` so dangling cleanup can target wrix-owned images without
touching user images. On Darwin, a successful archive load is tagged with its
stable wrix ref and its temporary Apple `untagged@sha256:<digest>` load ref is
removed immediately. Retention also recognizes historical untagged records as
cleanup candidates when their image variant satisfies the wrix-managed label
contract owned by `image-builder.md`. On Linux, legacy tagged `localhost/wrix-*`
images may be removed when outside the keep set. Unlabelled dangling images are
not automatically removed on either platform because ownership is ambiguous;
Wrix may report those images and offer a manual/opt-in cleanup path.

### Boundary Class

- macOS: microVM via Virtualization.framework, always
- Linux: rootless container by default; `WRIX_MICROVM=1` opts into
  `podman --runtime krun` when `/dev/kvm` is available. krun is bundled via Nix;
  without KVM the opt-in fails loudly rather than silently degrading.

The host Podman API is outside the normal sandbox boundary. Linux exposes it
only through the explicit unsafe operator opt-in `WRIX_UNSAFE_PODMAN_SOCKET`,
which mounts the host user's Podman socket and exports `CONTAINER_HOST` for
sibling-container workflows. The legacy `WRIX_PODMAN_SOCKET` name has no effect.

Threat-model rationale for these choices lives in `specs/security.md`.

### Network Posture

`WRIX_NETWORK` selects public egress posture at launch time. The launcher passes
the mode, merged allowlist, DNS exceptions, and wrix-owned local endpoint
exceptions into the container via env/config; both platforms use an immutable
first-stage bootstrap to install an in-sandbox firewall ruleset before any
workspace setup or agent code runs. Linux Podman uses `nftables` by default.
Darwin does not use host `pf`; it uses the firewall backend available inside the
Linux guest/container (`nftables` when supported, otherwise a verified
equivalent such as iptables).

Baseline network isolation is always enforced in both modes: no inbound ports,
IPv6 disabled/blocked for v1, and outbound traffic to
LAN/private/host-local/VPN/special ranges is blocked. Exact exceptions are
allowed only for wrix-owned endpoints (for example the project cache
host-gateway IP/port, Darwin Dolt TCP endpoint) and configured DNS resolvers on
TCP/UDP port 53.

- `open` (default) — public-internet outbound is allowed, but
  LAN/private/host-local/VPN/special ranges remain blocked.
- `limit` — outbound is restricted to the profile's merged `networkAllowlist`
  plus exact wrix-owned local endpoint and DNS exceptions;
  LAN/private/host-local/VPN/special ranges remain blocked. Any other value
  errors at the launcher before the container starts.

Filtering is fail-closed. Both platforms grant temporary in-container
`NET_ADMIN` only for trusted startup. The immutable bootstrap uses only
image-pinned tools, verifies the namespace-local firewall policy, and replaces
itself through `capsh` with a stage that rejects `NET_ADMIN` in every Linux
capability set before workspace setup or exit logging is installed. Linux
rootless Podman uses this sequence for both the default container boundary and
optional `WRIX_MICROVM=1` boundary; macOS runs in a microVM unconditionally. The
Darwin host firewall is never mutated. `WRIX_NETWORK=limit` domains are resolved
once at startup; any unresolvable allowlist domain fails launch instead of being
silently omitted. If firewall setup, IPv6 disablement, or capability drop cannot
be verified, launch fails; wrix never falls back to LAN-open networking.

### Agent Runtime Axis

The `agent` parameter selects, **at build time**, the agent runtime whose
executable the entrypoint launches. That executable must be in the image, so
this is not a runtime knob.

- `pi` (default) — the packaged Pi coding agent; no consumer package required
- `claude` — `claude-code` from nixpkgs; no consumer package required
- `direct` — a consumer-supplied runner, requiring `agentPkg` with an explicit,
  nonempty, single-component `meta.mainProgram`

For `direct`, the entrypoint executes
`${agentPkg}/bin/${agentPkg.meta.mainProgram}` from the selected image, with
agent arguments and stdio preserved. Missing package or executable declaration
fails Nix evaluation; a missing executable fails before agent execution. There
is no placeholder runner or Wrix-mandated runner name. Wrix does not interpret
the consumer runner's stdio protocol.

The selector's closed set and runtime meanings are owned here. Physical image
composition and the scope of automatic runtime exclusivity are owned by
`image-builder.md` § Provenance-Tiered Layering. The agent tier composes
orthogonally with the profile, so variants are `(profile × agent)`.

#### Agent Selection

Selection is by build target, not by caller env. `WRIX_AGENT` is the internal
wire the entrypoint reads, but callers do not select it by exporting env vars. A
human selects an agent by choosing the `mkSandbox { agent = …; }` build / its
`sandbox-<profile>[-<agent>]` target; that choice is encoded in the immutable
`ProfileConfig` JSON. Orchestrators driving the raw `launcher` pass a matching
per-call `ProfileConfig`.

#### Entrypoint Agent Guards

The image declares its baked agent variant in `/etc/wrix/image-agent`. The
entrypoint dispatches on `WRIX_AGENT` and, before exec, first rejects a mismatch
between the ProfileConfig-selected agent and the image-declared agent with a
clear ProfileConfig/image-variant error, then verifies the named binary is
present (`command -v`). A request for an agent absent from the image — e.g.
`WRIX_AGENT=pi` against a claude image on the raw-launcher path — fails loudly
with a clear error instead of a bare `command not found`.

#### Per-Agent Configuration

Each agent keeps its own config system; Wrix delivers configuration rather than
abstracting it:

- _Config home_ — the entrypoint seeds the selected agent's config home from
  baked defaults: claude → `~/.claude`, pi → `~/.pi/agent`; `direct` has none.
  Session data persists in the documented workspace locations.
- _Credentials_ — API keys and OAuth/subscription tokens reach the agent through
  declared `runtimeSecrets`, same-named host env or `SpawnConfig.env`, and
  credential-file mounts. Static profile/mkSandbox env is non-secret and
  image-baked. The credential invariants are owned by `security.md`.
- _Package/settings overrides_ — `agentPkg` overrides the selected agent
  package; `agentSettings` merges into the selected agent's settings schema. For
  `agent = "direct"`, omitted or empty `agentSettings` is accepted; nonempty
  settings are rejected because direct has no Wrix-owned settings schema.

### Pi Settings

Wrix seeds native Pi settings and honors consumer `agentSettings` overrides.
Additive codemode is enabled by default with `defaultTools = [ "+codemode" ]`
and `codemode.mode = "on"`, including when no MCP servers are selected. Read,
Bash, edit, and write remain directly available. Consumers can replace these
defaults through Pi's own settings; Wrix adds no codemode interpreter or
tool-orchestration abstraction. Model selection, reasoning level, and cosmetic
defaults are configuration choices, not fixed sandbox contracts. Pi defaults to
`defaultProjectTrust = "always"` inside the container and
`enableInstallTelemetry = false`; neither setting replaces the container
security boundary. Session persistence uses an explicit
`/workspace/.pi/agent/sessions` directory rather than importing arbitrary
workspace agent-home files.

### MCP Servers

`mkSandbox`'s `mcp` parameter opts registered servers in per sandbox, for
example `mcp.playwright = { … }`; `playwright-mcp.md` owns that server's
contract. Native terminal debugging is owned by `tmux.md`, not an MCP server.
Wrix normalizes selected stdio servers into a schema-v1 manifest whose entries
carry `name`, `command`, `args`, and `env`, exporting its path as
`WRIX_MCP_MANIFEST` for every agent. Explicit `mcp` selects its declared
servers. `mcpRuntime = true` bakes every registered server and selects them at
launch through `WRIX_MCP`: unset or `all` selects all, an empty value selects
none, and a comma-separated list selects named servers. Unknown names fail
startup.

Wrix translates the selected manifest into native client configuration: Claude's
`mcpServers` and Pi's container-local `~/.pi/agent/mcp.json`. Generated Pi
configuration contains only the current selection and is regenerated on launch,
never copied back into host configuration. Trusted project `.pi/mcp.json`
follows Pi's native precedence, including same-name overrides; `WRIX_MCP`
controls Wrix-managed selection, not arbitrary project capabilities.

Pi owns MCP transport, discovery, namespacing, cancellation, shutdown, and
result presentation. Wrix exposes Pi-native names such as
`mcp__playwright__browser_snapshot`, without unqualified aliases or a custom
protocol client. Wrix-managed Pi servers use native `codemode` exposure;
codemode receives complete MCP results, including `structuredContent`,
`content`, and `isError`, while direct presentation follows Pi's own rules.
Direct runners receive the unchanged manifest handoff and own its consumption.
Profile output naming remains in `profiles.md`.

## mkSandbox API

```nix
mkSandbox {
  profile = profiles.base;          # Workspace profile (profiles.md). Default: base
  cpus = null;                      # CPU limit honored by the platform launcher
  memoryMb = 4096;                  # Memory limit MB
  deployKey = "myproject";          # Default key identity, not permission to mount keys
  packages = [ pkgs.jq ];           # Extra packages merged into profile.packages
  mounts = [ {                      # Extra mounts merged into profile.mounts
    source = "~/.config";
    dest = "/home/wrix/.config";
    mode = "ro";                    # "ro" or "rw"
  } ];
  env = { FOO = "bar"; };           # Non-secret env merged into profile.env
  runtimeSecrets = {                 # Runtime values resolved by the host launcher
    OPENAI_API_KEY = "required";     # "required" or "optional"
  };
  mcp.playwright = { };             # MCP server opt-in
  mcpRuntime = false;               # Bake ALL MCP servers, defer selection to entrypoint
  agent = "pi";                     # "pi" (default), "claude", or "direct"
  agentPkg = null;                  # Required with meta.mainProgram for direct
  agentSettings = { };              # Settings for the selected agent
}
```

Returns `{ package, image, launcher, profile, devShell }`.

## Launcher Runtime Contract

`cli.md` owns that the launcher is exposed as `wrix run` and `wrix spawn`,
including help and root dispatch. This section owns the runtime contract behind
those subcommands. The Rust `wrix` launcher binary is profile-agnostic. Nix
supplies build-time defaults through an immutable `ProfileConfig` JSON file,
passed by `--profile-config <path>` or a wrapper-set equivalent. Both launcher
subcommands share container construction (mounts, env passthrough, runtime
selection, deploy key, workspace service startup, network firewall
configuration); they differ only in stdio and per-launch configuration source.

Git grants are resolved independently for `deploy` and `sign`: explicit launch
override, then `[wrix.git]` in the selected workspace repository's root
`wrix.toml`, then `false`. Outside a repository there is no repository policy
tier. Repository policy is read anew for each launch. The trusted caller can
grant or remove either capability. `cli.md` owns repository policy names;
`security.md` owns the repository-policy trust boundary, key resolution, and
credential exposure. Profiles select key identity, not grants.

`wrix run` accepts `--git-deploy` / `--no-git-deploy` and `--git-sign` /
`--no-git-sign` as invocation-only launcher options. Supplying both sides of a
pair in the launcher-option prefix is an error. Parsing launcher options stops
at the first positional argument or `--`, whichever comes first. A positional
argument before `--` selects the workspace; without one, the workspace is CWD.
After the workspace, one optional leading `--` is consumed and all remaining
arguments are passed unchanged to the agent, even if they match Wrix option
names. With no workspace argument, `--` is required to pass agent arguments.

For a configured launcher, `wrix run --git-sign /repo -- --help` grants signing
and passes `--help` to the agent in `/repo`. `wrix run --no-git-sign -- --help`
uses CWD and disables signing. `AGENT_ARGS` are the selected agent's argv, not a
Wrix-interpreted shell command.

`wrix spawn` uses optional `SpawnConfig.git.deploy` and `SpawnConfig.git.sign`
booleans; omitted fields inherit independently. Invalid policy or override types
fail before service startup or credential staging. `WRIX_GIT_SIGN` is not a
policy input.

Before launching the agent container, `wrix` ensures the per-workspace service
container (`<repo>-service`) is running when beads or the project Nix cache is
enabled. Dolt endpoints and project-cache `NIX_CONFIG` injection are owned by
`services.md`; this spec owns only that both launcher subcommands use the same
container construction path.

| Subcommand                                           | Stdio                         | Configuration source                                 | Use case                                            |
| ---------------------------------------------------- | ----------------------------- | ---------------------------------------------------- | --------------------------------------------------- |
| `wrix run [LAUNCH_OPTIONS] [DIR] [--] [AGENT_ARGS…]` | TTY (`-it`)                   | `ProfileConfig` JSON + host env + CLI args           | Interactive sessions, `nix run .#sandbox-<profile>` |
| `wrix spawn --spawn-config <file> [--stdio]`         | Non-TTY; stdin with `--stdio` | `ProfileConfig` JSON + per-launch `SpawnConfig` JSON | Programmatic dispatch (loom; future orchestrators)  |

`wrix spawn` stays in the foreground until the container command finishes and
returns its observed exit status. It does not allocate a TTY. `--stdio` enables
container stdin and requests the selected agent's stdio mode; it does not
detach. Callers may background the launcher themselves, but Wrix does not take
over its supervision. Execution-record completion follows `security.md`.

`ProfileConfig` JSON is generated by Nix into the store and contains the
immutable profile/image defaults. It is data, not shell code, and the Rust CLI
validates it before constructing platform container argv. Schema v1:

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
    "source_kind": "<platform-source-kind>",
    "digest": "sha256:..."
  },
  "agent": {
    "kind": "pi"
  },
  "resources": {
    "cpus": null,
    "memory_mb": 4096,
    "pids_limit": 4096
  },
  "security": {
    "deploy_key": null,
    "runtime_secrets": {
      "ANTHROPIC_API_KEY": "optional",
      "CLAUDE_CODE_OAUTH_TOKEN": "optional",
      "OPENAI_API_KEY": "optional"
    }
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

`profile.mounts` are profile-level mounts and are additive with
`mkSandbox.mounts` and `SpawnConfig.mounts`. `profile.env` is non-secret
profile/default container environment; it enters the Nix store and OCI image
metadata and is overridden only by explicit per-launch env rules.
`image.source_kind` uses the stable values and meanings defined by
`image-builder.md`; this spec owns validation and install dispatch, not those
values. `security.runtime_secrets` maps validated environment names to
`optional` or `required` and contains no values; the launcher resolves values
from `SpawnConfig.env` or same-named host env immediately before launch.
`agent.kind` is one of `direct`, `claude`, or `pi`; callers may not change it
independently of `image`. `services.beads.enable = "auto"` means start Dolt when
the workspace has `.beads/dolt`; `services.nix_cache.enable` controls
project-cache endpoint injection. `network.default_mode` defaults to `open` and
may be overridden at launch by `WRIX_NETWORK=open|limit`; both modes keep the
local-network isolation baseline.

`SpawnConfig` JSON has stable top-level fields:

- `image_ref` — optional per-launch image ref override; when absent,
  `ProfileConfig.image.ref` is used.
- `image_source` — optional per-launch Nix store path of the image source; when
  absent, `ProfileConfig.image.source` is used.
- `image_source_kind` — optional per-launch source-kind override under the
  `image-builder.md` source-kind contract; when absent,
  `ProfileConfig.image.source_kind` is used. If `image_source` is present,
  `image_source_kind` must also be present, even when it matches the profile
  kind, so source overrides never rely on launcher inference. For an
  `image_source` override, the installer derives and validates the selected
  image digest from that override source before preflight instead of reusing
  `ProfileConfig.image.digest`. The installer installs the selected image into
  the platform store before the launcher invokes the container CLI; preflight +
  dispatch semantics are documented in _Image install path_ above.
- `workspace` — host path bind-mounted at `/workspace`
- `env` — per-launch `[key, value]` pairs to pass through; pairs matching
  `security.runtime_secrets` are runtime credential sources and never enter Nix
  or image data
- `agent_args` — argv tail passed to the agent binary
- `git` — optional object with independently optional boolean `deploy` and
  `sign` overrides; absence inherits repository policy, explicit `false`
  disables the corresponding grant, and explicit `true` enables it
- `mounts` — optional `[{host_path, container_path, read_only}]` list; omitted
  or empty means no per-launch mounts. Additive to `profile.mounts` and
  `mkSandbox.mounts`.

Plus consumer-defined fields the entrypoint reads from the original config
mounted read-only inside the container at the path named by `WRIX_SPAWN_CONFIG`.
The schema is part of the launcher runtime contract, and CLI help mirrors it per
`cli.md`. Per-launch `SpawnConfig` may override launch-time inputs (workspace,
env allowlist, agent args, Git grants, mounts, image ref/source for
orchestrators), but it may not change the selected agent independently of the
image/profile config. `wrix run` errors when no valid `ProfileConfig` is
supplied; there is no implicit default image baked in.

## Platform Implementations

### Linux (Podman)

- Rootless Podman is the default Linux runtime; krun is optional.
- The launcher grants temporary in-container `NET_ADMIN` for firewall setup on
  every launch, including `WRIX_NETWORK=open`, because baseline
  LAN/private/host-local/VPN blocking is always required. The immutable
  bootstrap installs the in-sandbox firewall ruleset (`nftables` by default on
  Linux Podman), disables/blocks IPv6 for v1, verifies policy, and replaces
  itself through `capsh` before workspace-controlled setup, the agent, or exit
  logging can execute.
- Default boundary runs as rootless **container-root** (no `--userns=keep-id`),
  which maps to the invoking host user — the owner of the baked `/nix/store` —
  so store-mutating Nix ops succeed and `/workspace` files carry host UID/GID.
  The launcher sets `IS_SANDBOX=1` so claude permits
  `--dangerously-skip-permissions` while the process remains root, and does not
  preload `libfakeuid`. The microVM path keeps `--userns=keep-id` (krun maps
  host user→root inside the VM) and enters the same trusted bootstrap through
  `krun-init.sh`; argv decoding and libfakeuid activation occur only after the
  capability drop.
- `--pids-limit 4096` fork-bomb guard
- `WRIX_MICROVM=1` switches to `podman --runtime krun` when `/dev/kvm` exists

### macOS (Apple `container` CLI)

- Requires macOS 26+ and Apple Silicon
- Virtualization.framework microVM, always (no separate container-mode path)
- vmnet networking with the same always-on in-guest firewall policy: no inbound
  ports, public-internet outbound in `open`, allowlist-only outbound in `limit`,
  LAN/private/host-local/VPN/special ranges blocked in both modes, IPv6
  disabled/blocked for v1. The Darwin host `pf` firewall is not part of the
  sandbox contract.
- An immutable `/network-bootstrap.sh` is the only stage granted `NET_ADMIN`; it
  uses image-pinned binaries, verifies the firewall, and `exec`s
  `/entrypoint.sh` through `capsh` after dropping `NET_ADMIN`. The agent
  entrypoint fails before workspace setup unless the trusted bootstrap marker
  exists and `NET_ADMIN` is absent from inheritable, permitted, effective,
  bounding, and ambient capability sets.
- VirtioFS workspace mount
- Mount classifier handles `profile.mounts` and `SpawnConfig.mounts` uniformly —
  directories staged + copied at launch, regular files copy-from-parent-dir,
  Unix-socket sources rejected at launch
- Entrypoint creates user matching host UID

## Success Criteria

- `mkSandbox` accepts the documented parameter set (`profile`, `cpus`,
  `memoryMb`, `deployKey`, `packages`, `mounts`, `env`, `runtimeSecrets`, `mcp`,
  `mcpRuntime`, `agent`, `agentPkg`, `agentSettings`) and returns
  `{ package, image, launcher, profile, devShell }`
  [check](verify:sandbox.mksandbox-api)
- Platform dispatch picks the Linux implementation on Linux hosts and the macOS
  implementation on Darwin hosts [check](verify:sandbox.platform-dispatch)
- Evaluating `mkSandbox` on an unsupported system throws at evaluation time
  rather than producing a broken derivation
  [check](verify:sandbox.unsupported-system-error)
- A built Linux sandbox starts a container and exits cleanly
  [system](verify:sandbox.linux-container-starts)
- A built macOS sandbox starts an Apple `container` microVM and exits cleanly
  [system](verify:sandbox.darwin-container-starts)
- The Darwin network bootstrap cannot resolve tools from `/workspace`, verifies
  the firewall before invoking `capsh`, preserves the agent argv, and enters
  stage two only after requesting an irreversible `NET_ADMIN` drop
  [system](verify:sandbox.darwin-network-bootstrap)
- Linux container and krun initialization install the same open/limit policy and
  exact endpoint exceptions with image-pinned tools, preserve argv, and drop
  `NET_ADMIN` before workspace shims run during setup, agent execution, or exit
  logging [system](verify:sandbox.linux-network-bootstrap)
- Both agent entrypoints reject a missing bootstrap marker or retained
  `NET_ADMIN` before setup or exit logging can run
  [system](verify:sandbox.entrypoint-requires-bootstrap)
- Files created inside `/workspace` carry the host UID/GID, not a
  container-internal UID [system](verify:sandbox.uid-mapping)
- Host filesystem outside `/workspace` and declared mounts is not visible inside
  the container [system](verify:sandbox.filesystem-isolation)
- `mounts` and `env` passed to `mkSandbox` are merged into the profile and reach
  the container as configured [system](verify:sandbox.custom-mounts-env)
- Every sandbox image carries the `wrix` CLI, so `wrix beads push` resolves
  inside the container without entering `nix develop` or running `nix run`
  [check](test-ci:test-wrix-cli-in-profile)
- In a fresh container built from a profile that ships `nix`, the runtime
  process (rootless container-root) runs `nix develop -c true`, a `nix build` of
  a flake target, and a store-mutating op against a baked root-owned path to
  completion (exit 0) with no `Operation not permitted` failure on a
  `/nix/store` path [system](verify:sandbox.nix-in-container)
- The default container boundary sets `IS_SANDBOX=1` so claude permits
  `--dangerously-skip-permissions` as root without UID spoofing, does not
  `LD_PRELOAD` `libfakeuid`, and keeps libfakeuid restricted to the krun path
  [test](../crates/wrix-sandbox/tests/launch.rs::linux_default_boundary_sets_is_sandbox_without_fakeuid)
- In a fresh container built from the selected image, the runtime user completes
  `nix-store --verify --check-contents` and an additive `nix build` without
  store-integrity failures. The baked store/database guarantee is owned by
  `image-builder.md` § In-Container Nix Store Consistency
  [system](verify:sandbox.nix-store-verify-clean)
- The launcher accepts exactly `WRIX_NETWORK=open` and `WRIX_NETWORK=limit`
  [test](command::launch::test::network_mode_parse_accepts_only_open_and_limit)
- Any other `WRIX_NETWORK` value errors through the production CLI before
  workspace services or a container start
  [test](../crates/wrix-cli/tests/sandbox_launch.rs::invalid_network_mode_fails_before_service_or_container_start)
- The configured network default is applied when `WRIX_NETWORK` is absent;
  explicit `open`/`limit` overrides take precedence
  [test](../crates/wrix-cli/tests/sandbox_launch.rs::profile_network_defaults_and_explicit_environment_precedence)
- Malformed network defaults and unsupported IPv6 policy fail before subprocess
  side effects, even with a valid environment override
  [test](../crates/wrix-cli/tests/sandbox_launch.rs::malformed_network_policy_fails_before_subprocesses_even_with_override)
- In `WRIX_NETWORK=open`, sandbox outbound to public internet succeeds, but
  outbound to LAN/private/host-local/VPN/special IPv4 ranges fails except for
  exact DNS and wrix-owned endpoint exceptions
  [system](verify:sandbox.network-open-blocks-lan)
- In `WRIX_NETWORK=limit`, outbound succeeds only to the merged allowlist plus
  exact DNS and wrix-owned endpoint exceptions; allowlist domains are resolved
  once at startup, unresolvable domains fail launch, and non-allowlisted public
  internet plus LAN/private/host-local/VPN/special ranges fail
  [system](verify:sandbox.network-limit-allowlist)
- IPv6 egress is disabled or blocked in both network modes for v1
  [system](verify:sandbox.network-ipv6-blocked)
- If firewall setup, IPv6 disablement, or `NET_ADMIN` drop cannot be verified,
  the launcher fails closed before the agent starts and never falls back to
  LAN-open networking [system](verify:sandbox.network-fail-closed)
- After startup, the agent process cannot modify firewall rules (for example
  `nft flush ruleset` on the nft backend, or the equivalent backend flush
  command, fails inside the running sandbox)
  [system](verify:sandbox.agent-lacks-net-admin)
- `WRIX_MICROVM=1` selects `podman --runtime krun --userns=keep-id` on Linux
  when `/dev/kvm` is available, enters through the relay, serializes the
  requested command through `WRIX_KRUN_CMD`, passes terminal dimensions for PTY
  setup, and reaches the krun-only init/libfakeuid boundary
  [system](verify:sandbox.linux-microvm-runtime)
- MicroVM opt-in fails with the typed missing-KVM error before Podman runtime
  dispatch when the KVM device is absent
  [system](verify:sandbox.linux-microvm-missing-kvm)
- `wrix run` errors at startup with a clear message when no valid Nix-generated
  `ProfileConfig` JSON is supplied
  [test](../crates/wrix-sandbox/tests/command.rs::run_requires_valid_profile_config)
- `mkSandbox`'s `package` wrapper keeps `bin/wrix` explicit, exposes `wrix-run`
  as `meta.mainProgram` for `nix run`, and passes an immutable Nix-store
  `ProfileConfig` JSON path to the profile-agnostic launcher for both `run` and
  `spawn`, with image defaults supplied by `ProfileConfig` rather than mutable
  `WRIX_DEFAULT_IMAGE_*` env vars [check](test-ci:test-profile-config-wrapper)
- `ProfileConfig.image` includes `ref`, `source`, explicit `source_kind`, and
  `digest`; the launcher/runtime installer rejects configs where `source_kind`
  is missing or incompatible with the selected platform install path
  [check](test-ci:test-profile-config-image-source-kind)
- Complete `ProfileConfig` parsing rejects malformed fields before subprocesses
  [test](../crates/wrix-cli/tests/sandbox_launch.rs::malformed_profile_config_fails_before_subprocesses)
- Complete `ProfileConfig` parsing rejects duplicate JSON fields before
  subprocesses
  [test](../crates/wrix-cli/tests/sandbox_launch.rs::duplicate_profile_config_fields_fail_at_the_json_boundary)
- Complete `ProfileConfig` parsing retains intentional wire extensibility and
  platform-specific source semantics
  [test](../crates/wrix-sandbox/src/command/config.rs::profile_boundary_preserves_extension_fields_and_typed_source_semantics)
- Image installer construction rejects empty source paths and contradictory
  runtime/source combinations before store operations
  [test](../crates/wrix-sandbox/src/image.rs::installation_constructor_rejects_contradictory_sources)
- The selected agent runtime comes from `ProfileConfig` and cannot be changed by
  caller env independently of the selected image/profile
  [test](../crates/wrix-sandbox/tests/command.rs::profile_config_agent_cannot_be_overridden_by_env)
- `wrix spawn --spawn-config <file>` parses the core `SpawnConfig` fields
  (`image_ref`, `image_source`, `image_source_kind`, `workspace`, `env`,
  `agent_args`, `mounts`) into the launch plan
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::documented_spawn_config_fields_render_into_launch_plan)
- With or without `--stdio`, `wrix spawn` runs without a TTY or detachment,
  waits for the container command to finish, and returns its observed exit
  status; `--stdio` enables stdin rather than changing the lifecycle
  [test?](../crates/wrix-sandbox/tests/launch.rs::spawn_waits_for_container_completion)
- `SpawnConfig.git` parses independent boolean grants, preserving omission as
  inheritance and explicit false as an override rather than collapsing them
  [test?](../crates/wrix-sandbox/tests/spawn_config.rs::git_grants_preserve_omission_and_explicit_false)
- A `SpawnConfig.image_source` override requires an explicit source kind
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::image_source_override_requires_source_kind)
- A `SpawnConfig.image_source_kind` override must be compatible with the current
  platform
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::image_source_kind_must_match_platform)
- `SpawnConfig` cannot change the selected agent independently of
  `ProfileConfig`
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::spawn_config_cannot_override_agent)
- Consumer-defined `SpawnConfig` fields remain available to the in-container
  entrypoint through the read-only config mount and `WRIX_SPAWN_CONFIG`
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::consumer_spawn_config_fields_are_mounted_for_entrypoint)
- On Linux, each `SpawnConfig.mounts` entry becomes a
  `-v <host_path>:<container_path>` podman argument, with `:ro` appended when
  `read_only: true`. A missing or empty `mounts` list produces no additional
  `-v` flags.
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::linux_spawn_mounts_render_podman_volume_args)
- The packaged launcher does not mount the host Podman socket or export
  `CONTAINER_HOST` / `GC_HOST_*` by default; `WRIX_PODMAN_SOCKET` is ignored;
  and only `WRIX_UNSAFE_PODMAN_SOCKET` renders a real socket mount plus
  host-visible `CONTAINER_HOST` / `GC_HOST_*`, failing loudly when the socket is
  absent [system](verify:sandbox.unsafe-podman-socket)
- On Darwin, the same mount classifier handles `profile.mounts` and
  `SpawnConfig.mounts` — one mechanism, not two. Directories are staged + copied
  at launch and regular files copy from their parent directory.
  [test](../crates/wrix-sandbox/tests/darwin_mounts.rs::mount_classifier_handles_profile_and_spawn_mounts_uniformly)
- On Darwin, mount entries whose `host_path` is a Unix socket fail before the
  container starts because VirtioFS does not pass socket operations
  [test](../crates/wrix-sandbox/tests/darwin_mounts.rs::mount_classifier_rejects_unix_sockets)
- Omitted `agent` selects Pi; explicit `direct` requires a package and a valid
  `meta.mainProgram` declaration rather than supplying a placeholder
  [check](verify:sandbox.agent-default-and-direct-contract)
- Both entrypoints execute the selected Pi, Claude, or declared direct
  executable; a non-Loom-named direct runner receives the original arguments and
  bidirectional stdio, and a missing direct executable fails clearly
  [system?](verify:sandbox.entrypoint-declared-runner)
- Both launch modes read current repository policy on each launch and resolve
  independent Git grants from launch override, repository policy, then false,
  without treating key identity, host key presence, or `WRIX_GIT_SIGN` as a
  grant or policy override
  [test?](../crates/wrix-sandbox/tests/launch.rs::git_grants_follow_override_repo_default_precedence)
- `wrix run` parses launcher options only before the workspace or `--`, uses CWD
  when the workspace is omitted, consumes the documented separator, and forwards
  the remaining agent arguments unchanged, including Wrix-looking option names
  and agent `--help`
  [test](../crates/wrix-cli/tests/sandbox_launch.rs::run_launch_options_stop_before_agent_arguments)
- Launch overrides reject conflicting flag pairs and malformed Git policy,
  including the unsupported `sign_commits` key, before services or credential
  staging
  [test?](../crates/wrix-cli/tests/sandbox_launch.rs::invalid_git_policy_fails_before_side_effects)
- Before exec'ing the selected agent, the entrypoint rejects a mismatch between
  the ProfileConfig-selected `WRIX_AGENT` and the image-declared
  `/etc/wrix/image-agent`, then verifies the agent's binary is present and fails
  loudly with a clear error when it is absent from the image (e.g.
  `WRIX_AGENT=pi` against a claude image), rather than emitting a bare
  `command not found` [system](verify:sandbox.agent-binary-guard)
- Both entrypoints seed the selected agent's config home — claude `~/.claude`,
  pi `~/.pi/agent` — and make its documented workspace session location
  available [system](verify:sandbox.agent-config-homes)
- Granted deploy and signing keys are mounted independently and read-only at
  `/etc/wrix/keys/<name>` and `/etc/wrix/keys/<name>-signing`, respectively;
  child key environment variables point only at those mounted paths, and neither
  host-source paths nor `.pub` files are delivered
  [test?](../crates/wrix-sandbox/tests/launch.rs::independent_git_grants_use_fixed_private_key_destinations)
- `ProfileConfig.security.deploy_key` accepts only a validated, single-component
  deploy-key name; absolute paths, separators, whitespace, and dot traversal
  fail during config parsing before credential staging
  [test](../crates/wrix-sandbox/tests/launch.rs::profile_config_rejects_unsafe_deploy_key_names_before_staging)
- Both entrypoints can derive the deploy public key from the mounted private key
  on demand with `ssh-keygen -y`
  [system](verify:sandbox.entrypoint-deploy-key-public)
- `agentSettings` merges into the selected agent's baked settings; direct
  accepts omission or `{}` and rejects nonempty settings at evaluation time
  [check](test-ci:test-sandbox-agent-settings)
- Pi's baked security-relevant defaults enable project trust inside the sandbox
  and disable install telemetry; update checking is a separate setting
  [check](test-ci:test-sandbox-agent-settings)
- Pi settings delivery preserves unspecified defaults and applies explicit
  consumer model, display, and tool-setting overrides without requiring any
  particular model ID or cosmetic value
  [check?](verify:sandbox.pi-settings-precedence)
- Packaged Pi exposes additive codemode alongside read, Bash, edit, and write
  with no MCP servers configured, executes a built-in-tool script successfully,
  and honors an explicit consumer override disabling codemode
  [system?](verify:sandbox.pi-codemode-tools)
- When `/workspace/bin` exists inside the container, it appears first on `PATH`,
  so a consumer-supplied shim at `/workspace/bin/<name>` resolves ahead of a
  same-named binary baked into the image
  [system](verify:sandbox.workspace-bin-path-present)
- When `/workspace/bin` does not exist, the container's `PATH` does not contain
  `/workspace/bin` [system](verify:sandbox.workspace-bin-path-absent)
- Both platform entrypoints implement the `/workspace/bin` PATH prepend
  [system](verify:sandbox.entrypoint-workspace-bin-prepend)
- The packaged runtime image installer preflight checks whether the selected
  image source's content digest matches any image already present in the
  platform store before invoking the install pipeline; on a digest hit, no image
  source is executed, no tar bytes are streamed, and no `*-load` CLI is invoked
  [system](test-ci:test-image-install-digest-skip)
- On Linux, the runtime image installer dispatches the Linux source kind defined
  by `image-builder.md` through an archive-less descriptor-to-OCI-layout install
  path (`oci:<oci_layout>:<oci_ref>` → `containers-storage:<ref>` with skopeo,
  or equivalent wrix); the docker/OCI archive conversion path is not used for
  Linux descriptor sources
  [test](../crates/wrix-sandbox/tests/image_install.rs::linux_descriptor_sources_use_archiveless_install_path)
- The packaged Linux launcher supplies a readable OCI layout to skopeo with the
  Podman store destination and skips installation when that image is already
  present [system](test-ci:test-image-install-real-skopeo)
- Digest preflight also works when the digest comes from the descriptor rather
  than ProfileConfig
  [test](../crates/wrix-sandbox/tests/image_install.rs::descriptor_digest_preflight_works_without_profile_digest)
- A second spawn of an already-loaded image performs no writes to the platform
  store's layer directory and does not execute the image source
  [test](../crates/wrix-sandbox/tests/image_install.rs::already_loaded_image_performs_no_store_writes)
- The runtime image cleanup path records a bounded cross-workspace MRU of eight
  typed wrix image refs/digests/image IDs, preserves images used by Podman
  containers, prunes wrix-managed images outside the keep set, and does not
  automatically remove unlabelled `<none>:<none>` images
  [test](../crates/wrix-sandbox/tests/image_retention.rs::cleanup_prunes_only_wrix_managed_images_outside_bounded_keep_set)
- Image MRU serialization preserves typed refs, digests, and IDs while accepting
  documented legacy empty fields
  [test](../crates/wrix-sandbox/src/image.rs::mru_round_trips_typed_identifiers_and_accepts_legacy_empty_fields)
- Runtime listings parse into typed store targets
  [test](../crates/wrix-sandbox/src/image.rs::podman_rows_parse_typed_references_ids_and_absent_fields)
- Concurrent launches update the shared MRU without losing either workspace's
  record or exposing partially-written JSON
  [test](../crates/wrix-sandbox/tests/image_retention.rs::concurrent_mru_updates_preserve_each_workspace_record)
- Apple `container list` records are parsed for image references and descriptor
  IDs so cleanup preserves images used by existing Apple containers
  [test](image::test::apple_container_list_preserves_images_used_by_existing_containers)
- Apple image digest inspection accepts prefixed digests, bare digests, and
  content-digest IDs, without treating opaque runtime IDs as digest evidence
  [test](../crates/wrix-sandbox/src/image.rs::apple_content_digest_accepts_prefixed_bare_and_id_fallback_variants)
- Runtime MCP selection reaches the container through `WRIX_MCP`
  [test?](../crates/wrix-sandbox/tests/launch.rs::runtime_mcp_selection_reaches_entrypoint)
- Explicit and runtime selection publish the same schema-v1 manifest for all
  agent kinds, preserve server command/arguments/environment mapping in native
  Claude and Pi configuration, and hand the manifest unchanged to direct runners
  [system?](verify:sandbox.mcp-manifest-handoff)
- Packaged Pi receives only the selected Wrix-managed entries, including
  none/all and unknown-name handling, uses native names and codemode exposure,
  replaces stale generated configuration on launch, leaves host configuration
  untouched, and independently honors native trusted-project MCP precedence
  [system?](verify:sandbox.pi-mcp-selection)
- Packaged Pi codemode receives structured MCP results without Wrix flattening
  or size-dependent shape changes; large results remain available to scripts
  under Pi's native result contract
  [system?](verify:sandbox.pi-mcp-structured-results)
- Packaged Pi can forward an MCP image block from a codemode call to its result
  without converting the image to prose [system?](verify:sandbox.pi-mcp-images)
- Packaged Pi preserves MCP tool errors as `isError` results in codemode and
  propagates protocol/transport failures and cancellation rather than reporting
  success or replaying side-effecting calls through a Wrix client
  [system?](verify:sandbox.pi-mcp-errors-cancellation)
- Native Pi owns selected stdio-server shutdown, including child processes; Wrix
  images contain no custom Pi MCP protocol client
  [system?](verify:sandbox.pi-mcp-lifecycle)
- On Darwin, the runtime image installer converts the Darwin source kind defined
  by `image-builder.md` to a temporary OCI archive before invoking
  `container image load --input <oci-archive>`, then removes the temporary
  archive and relies on digest-skip preflight while per-blob install remains out
  of scope [system](verify:sandbox.darwin-image-load)

## Requirements

### Functional

1. **mkSandbox API** — accepts the parameters above; returns
   `{ package, image, launcher, profile, devShell }`. Profile schema lives in
   `profiles.md`; image build in `image-builder.md`; MCP server contracts in
   `playwright-mcp.md`; native terminal debugging in `tmux.md`.
2. **Platform dispatch** — Linux selects the Podman launcher; macOS selects the
   Apple `container` CLI launcher; unsupported systems throw.
3. **Workspace mount** — the selected workspace bind-mounts at `/workspace`;
   profile mounts merge on top.
4. **UID mapping** — files created in `/workspace` carry host UID/GID.
5. **Custom mounts and env** — `mkSandbox`'s `mounts`, non-secret `env`, and
   `runtimeSecrets` extend the profile rather than replace it. Runtime-secret
   maps right-merge by environment name; values are resolved only by the
   launcher.
6. **Git key identity and delivery** — `deployKey` supplies a default validated
   key name, overridden by repository `wrix.git.deploy_key`; without either, the
   name is derived as specified by `cli.md`. Identity does not grant
   credentials. An effective deploy grant mounts only the deploy key at
   `/etc/wrix/keys/<name>`; an effective sign grant mounts only the signing key
   at `/etc/wrix/keys/<name>-signing`. Each mounted key gets its corresponding
   child `WRIX_DEPLOY_KEY` or `WRIX_SIGNING_KEY` value. Public keys can be
   derived with `ssh-keygen -y`; `.pub` files are not mounted. Host-source
   resolution and missing-key behavior belong to `security.md`.
7. **MCP opt-in** — Wrix owns registry selection and the
   manifest-to-native-config translation described in Architecture. Agent
   clients own protocol handling and presentation; external direct runners own
   manifest consumption. Profile output naming remains in `profiles.md`.
8. **Agent runtime axis** — Selection is build-time and immutable, with Pi as
   default and an explicit consumer package for direct. Configuration delivery,
   native Pi codemode, security-relevant defaults, and executable guards follow
   Architecture. Only the selected agent receives its config. `agentSettings`
   merges into native settings; direct accepts only omitted or empty settings.
   Credential delivery belongs to `security.md`. Image composition belongs to
   `image-builder.md`.
9. **Launcher contract** — `wrix run` reads immutable Nix-generated
   `ProfileConfig` JSON plus CLI/host-env runtime inputs; `wrix spawn` reads the
   same `ProfileConfig` plus per-launch `SpawnConfig` JSON. Both share container
   construction, including workspace service startup and endpoint injection when
   services are enabled. Wrapper config-generation rules are owned by
   _Architecture > Sandbox Outputs_; workspace service contracts are owned by
   `services.md`.
10. **Image source dispatch** — image install dispatches on the explicit source
    kinds owned by `image-builder.md`, not filename or platform guessing. A
    per-launch `image_source` override must carry a matching
    `image_source_kind`; it may not silently inherit an incompatible kind from
    `ProfileConfig`. The selected source's digest is derived or validated before
    preflight.
11. **Image retention** — runtime image cleanup is wrix-scoped and bounded: keep
    the image selected for the current operation, images used by existing
    containers, and eight shared cross-workspace MRU records (ref plus
    digest/image ID when available) before pruning; prune wrix-managed images
    outside the keep set; never automatically delete unlabelled `<none>:<none>`
    images.
12. **Per-launch mounts via SpawnConfig** — `wrix spawn`'s `SpawnConfig.mounts`
    adds per-launch bind mounts on top of `profile.mounts` and `mkSandbox`'s
    `mounts`. Each entry maps `host_path → container_path` with
    `read_only: true` rendering `:ro`. On Linux this is a literal `-v` flag. On
    Darwin, `SpawnConfig.mounts` flows through the same mount classifier as
    `profile.mounts`: directories staged + copied, regular files
    copy-from-parent-dir, Unix-socket sources rejected at launch with a clear
    error (VirtioFS does not pass socket operations). The launcher does not
    validate that `host_path` exists; podman fails at runtime if it does not.
13. **Workspace `bin/` PATH prepend** — When `/workspace/bin` exists inside the
    container, both Linux and macOS entrypoints prepend it to `PATH` so
    consumer-supplied shims under the workspace's `bin/` resolve ahead of
    image-baked binaries with the same name. The check is directory existence,
    not per-binary; the consumer owns what it ships in `bin/`. When the
    directory is absent, `PATH` is unchanged. The contract is PATH ordering only
    — wrix does not create `/workspace/bin`, does not validate its contents, and
    does not allowlist individual shims.
14. **In-container Nix** — a sandbox built from a `nix`-shipping profile lets
    the runtime user run both additive (`nix develop`, `nix build` of new
    closures) and store-mutating (replace, GC, delete of baked paths) Nix
    operations without permission or store-integrity failures. On the default
    boundary the runtime process is rootless container-root, which maps to the
    host user that owns the baked store, so it can mutate root-owned store paths
    — the `deletePath → fchmodat2(u+w)` primitive no longer hits `EPERM`. This
    spec owns that runtime identity and permissions boundary; `image-builder.md`
    § In-Container Nix Store Consistency owns the baked store/database
    invariant.

### Non-Functional

1. **Rootless / no elevated privileges** — Linux runs rootless Podman; macOS
   runs the Apple `container` CLI as the calling user. No host capabilities are
   granted by default; `WRIX_UNSAFE_PODMAN_SOCKET` is an explicit unsafe opt-in
   outside the normal sandbox boundary.
2. **Boundary class** — macOS is always microVM; Linux defaults to rootless
   container, opts into microVM with `WRIX_MICROVM=1` (see `specs/security.md`).
3. **Network posture** — no inbound ports on either platform. In both
   `WRIX_NETWORK=open` and `WRIX_NETWORK=limit`,
   LAN/private/host-local/VPN/special outbound is blocked with exact DNS and
   wrix-owned endpoint exceptions only. `open` allows public-internet outbound;
   `limit` restricts public egress to the merged allowlist. Filtering is
   fail-closed and drops `NET_ADMIN` before workspace-controlled setup, agent
   execution, or exit logging.
4. **Near-native performance** — minimal overhead beyond the container/microVM
   boundary cost; krun adds ~100MB per microVM.

## Out of Scope

- A Wrix-managed detached launch mode or background completion observer; callers
  retain supervision of foreground launchers
- Windows support
- GPU passthrough
- Inbound port forwarding
- User-defined unsafe networking modes; wrix does not provide a LAN-open escape
  hatch
- Per-user multi-tenant sharing (sandboxes are single-user-per-host by design)
- Managed-environment APIs, external-harness tool routing, and Pi Durable
  integration; whole-agent sandboxing is the supported model for this scope
- Wrix-owned codemode interpreters, classifier workflows, and codemode-only
  defaults; Pi owns tool composition, and consumers may configure it natively
- Compatibility aliases for retired tmux MCP tools or Wrix's Pi MCP client
